mod ap2;
mod backend;
mod compat;
mod discovery;
mod pcm;
mod raop;
mod server;
mod target;

use crate::backend::BackendSession;
use crate::discovery::Discovery;
use clap::{Parser, Subcommand, ValueEnum};
use fozmo_airplay_protocol::{Metadata, default_control_socket};
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

pub use target::{AIRPLAY2_ACCESS_UNSUPPORTED_MESSAGE, AirPlayTarget};

pub const AIRPLAY_SAMPLE_RATE: u32 = 44_100;
pub const AIRPLAY_BIT_DEPTH: u8 = 16;
const AIRPLAY_DEVICE_VOLUME_EXPONENT: f32 = 2.0;

#[derive(Parser)]
#[command(name = "fozmo-airplay-helper", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Discover and print network AirPlay receivers.
    List {
        #[arg(long, default_value_t = 3)]
        wait_seconds: u64,
        #[arg(long)]
        json: bool,
    },
    /// Play WAV or raw stereo 44.1 kHz signed 16-bit little-endian PCM.
    Play {
        receiver_id: String,
        /// Input file, or `-` for stdin with --format pcm-s16le.
        path: PathBuf,
        #[arg(long, value_enum, default_value_t = InputFormat::Wav)]
        format: InputFormat,
        #[arg(long, default_value_t = 5)]
        discovery_seconds: u64,
        #[arg(long)]
        volume: Option<f32>,
    },
    /// Serve the versioned JSON + PCM Unix-socket protocol.
    Serve {
        /// Control socket. Defaults to FOZMO_AIRPLAY_SOCKET, then a temp path.
        #[arg(long)]
        socket: Option<PathBuf>,
        /// Disable launcher parent-pipe EOF supervision for manual debugging.
        #[arg(long)]
        ignore_stdin_eof: bool,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum InputFormat {
    Wav,
    PcmS16le,
}

fn main() {
    if let Err(error) = run(Cli::parse()) {
        eprintln!("fozmo-airplay-helper: {error}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        Commands::List { wait_seconds, json } => list(wait_seconds, json),
        Commands::Play {
            receiver_id,
            path,
            format,
            discovery_seconds,
            volume,
        } => play(receiver_id, path, format, discovery_seconds, volume),
        Commands::Serve {
            socket,
            ignore_stdin_eof,
        } => server::serve(
            socket.unwrap_or_else(default_control_socket),
            !ignore_stdin_eof,
        ),
    }
}

fn list(wait_seconds: u64, json: bool) -> Result<(), String> {
    let discovery = Discovery::start();
    thread::sleep(Duration::from_secs(wait_seconds));
    let receivers = discovery.receivers();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&receivers)
                .map_err(|error| format!("failed to encode receiver list: {error}"))?
        );
    } else if receivers.is_empty() {
        println!("No AirPlay receivers found.");
    } else {
        for receiver in receivers {
            let kind = match receiver.service_kind {
                fozmo_airplay_protocol::ServiceKind::Raop => "raop",
                fozmo_airplay_protocol::ServiceKind::AirPlay2 => "airplay2",
            };
            println!(
                "{}\t{}\t{}\t{}",
                receiver.id,
                kind,
                if receiver.online { "online" } else { "offline" },
                receiver.name
            );
        }
    }
    Ok(())
}

fn play(
    receiver_id: String,
    path: PathBuf,
    format: InputFormat,
    discovery_seconds: u64,
    volume: Option<f32>,
) -> Result<(), String> {
    let discovery = Discovery::start();
    let deadline = Instant::now() + Duration::from_secs(discovery_seconds);
    let target = loop {
        if let Some(target) = discovery.online_target(&receiver_id) {
            break target;
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "receiver {receiver_id:?} was not discovered; run `list` first"
            ));
        }
        thread::sleep(Duration::from_millis(100));
    };

    let mut session = BackendSession::open(target, Metadata::default(), volume)
        .map_err(|error| format!("failed to open AirPlay receiver: {error}"))?;
    let mut producer = session
        .producer
        .take()
        .ok_or_else(|| "AirPlay PCM producer was already attached".to_string())?;
    let alive = session.alive.clone();
    match format {
        InputFormat::Wav => feed_wav(&path, &mut producer, &alive)?,
        InputFormat::PcmS16le => feed_raw_pcm(&path, &mut producer, &alive)?,
    }
    // Drain the PCM ring and any transport-owned tail before Drop disconnects it.
    while !producer.is_empty() && alive.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(10));
    }
    session.drain_buffered_tail();
    drop(session);
    Ok(())
}

fn feed_wav(
    path: &PathBuf,
    producer: &mut compat::AudioProducer,
    alive: &std::sync::atomic::AtomicBool,
) -> Result<(), String> {
    let mut reader = hound::WavReader::open(path)
        .map_err(|error| format!("failed to open {}: {error}", path.display()))?;
    let spec = reader.spec();
    if spec.channels != 2
        || spec.sample_rate != AIRPLAY_SAMPLE_RATE
        || spec.bits_per_sample != 16
        || spec.sample_format != hound::SampleFormat::Int
    {
        return Err(format!(
            "WAV must be stereo, 44100 Hz, signed 16-bit PCM (got {} channels, {} Hz, {} bits, {:?})",
            spec.channels, spec.sample_rate, spec.bits_per_sample, spec.sample_format
        ));
    }
    for sample in reader.samples::<i16>() {
        let value =
            sample.map_err(|error| format!("invalid WAV sample: {error}"))? as f64 / 32768.0;
        push_play_sample(producer, alive, value)?;
    }
    Ok(())
}

fn feed_raw_pcm(
    path: &PathBuf,
    producer: &mut compat::AudioProducer,
    alive: &std::sync::atomic::AtomicBool,
) -> Result<(), String> {
    let mut input: Box<dyn Read> = if path.as_os_str() == "-" {
        Box::new(std::io::stdin())
    } else {
        Box::new(
            std::fs::File::open(path)
                .map_err(|error| format!("failed to open raw PCM {}: {error}", path.display()))?,
        )
    };
    let mut buffer = [0u8; 32 * 1024];
    let mut odd = None;
    loop {
        let read = input
            .read(&mut buffer)
            .map_err(|error| format!("failed to read raw PCM: {error}"))?;
        if read == 0 {
            break;
        }
        decode_s16le_chunks(&buffer[..read], &mut odd, |sample| {
            push_play_sample(producer, alive, sample as f64 / 32768.0)
        })?;
    }
    if odd.is_some() {
        return Err("raw PCM ended with an incomplete 16-bit sample".into());
    }
    Ok(())
}

fn decode_s16le_chunks(
    bytes: &[u8],
    odd: &mut Option<u8>,
    mut consume: impl FnMut(i16) -> Result<(), String>,
) -> Result<(), String> {
    let mut index = 0;
    if let Some(low) = odd.take() {
        if let Some(high) = bytes.first() {
            consume(i16::from_le_bytes([low, *high]))?;
            index = 1;
        } else {
            *odd = Some(low);
            return Ok(());
        }
    }
    while index + 1 < bytes.len() {
        consume(i16::from_le_bytes([bytes[index], bytes[index + 1]]))?;
        index += 2;
    }
    if index < bytes.len() {
        *odd = Some(bytes[index]);
    }
    Ok(())
}

fn push_play_sample(
    producer: &mut compat::AudioProducer,
    alive: &std::sync::atomic::AtomicBool,
    mut value: f64,
) -> Result<(), String> {
    loop {
        if !alive.load(Ordering::Relaxed) {
            return Err("AirPlay transport ended while playing input".into());
        }
        match producer.push(value) {
            Ok(()) => return Ok(()),
            Err(returned) => {
                value = returned;
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

pub fn device_volume_to_transport_volume(volume: f32) -> f32 {
    volume.clamp(0.0, 1.0).powf(AIRPLAY_DEVICE_VOLUME_EXPONENT)
}

pub fn transport_volume_to_device_volume(volume: f32) -> f32 {
    volume
        .clamp(0.0, 1.0)
        .powf(1.0 / AIRPLAY_DEVICE_VOLUME_EXPONENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_pcm_decoder_preserves_samples_split_at_any_byte_boundary() {
        let mut odd = None;
        let mut samples = Vec::new();
        decode_s16le_chunks(&[0x00], &mut odd, |sample| {
            samples.push(sample);
            Ok(())
        })
        .unwrap();
        decode_s16le_chunks(&[0x80, 0xff, 0x7f, 0x34], &mut odd, |sample| {
            samples.push(sample);
            Ok(())
        })
        .unwrap();
        decode_s16le_chunks(&[0x12], &mut odd, |sample| {
            samples.push(sample);
            Ok(())
        })
        .unwrap();
        assert_eq!(samples, vec![i16::MIN, i16::MAX, 0x1234]);
        assert_eq!(odd, None);
    }
}
