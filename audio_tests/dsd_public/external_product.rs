use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;
use std::time::Instant;

pub const EXTERNAL_EC_ENV: &str = "HM_DSD_EC";

pub struct ExternalProductRender {
    pub left_msb: Vec<u8>,
    pub right_msb: Vec<u8>,
    pub wire_rate: u32,
    pub sample_count: usize,
    pub elapsed_seconds: f64,
}

pub fn render_ec_dsd128(
    executable: &Path,
    preset: &str,
    input: &Path,
    output: &Path,
) -> Result<ExternalProductRender, String> {
    let started = Instant::now();
    let result = Command::new(executable)
        .arg(input)
        .arg("-o")
        .arg(output)
        .arg("--dsd")
        .arg("--dsd-rate")
        .arg("128")
        .arg("-p")
        .arg(preset)
        .env(EXTERNAL_EC_ENV, "1")
        .output()
        .map_err(|error| format!("could not start external product: {error}"))?;
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    if !result.status.success() {
        return Err(format!("external-product EC render failed: {diagnostic}"));
    }
    let lower = diagnostic.to_ascii_lowercase();
    if lower.contains("unknown preset") || lower.contains("using extremehybrid") {
        return Err(format!(
            "external product rejected preset {preset} and selected a fallback: {diagnostic}"
        ));
    }
    if !diagnostic.contains("DSD rate: 5644800 Hz (DSD128)") {
        return Err(format!(
            "external product did not confirm the DSD128 wire rate: {diagnostic}"
        ));
    }
    let parsed = parse_dsf(output)?;
    if parsed.wire_rate != 5_644_800 {
        return Err(format!(
            "external-product DSF declared {} Hz, expected 5644800 Hz",
            parsed.wire_rate
        ));
    }
    Ok(ExternalProductRender {
        left_msb: parsed.left_msb,
        right_msb: parsed.right_msb,
        wire_rate: parsed.wire_rate,
        sample_count: parsed.sample_count,
        elapsed_seconds: started.elapsed().as_secs_f64(),
    })
}

pub fn write_stereo_float_wav(
    path: &Path,
    sample_rate: u32,
    left: &[f64],
    right: &[f64],
    gain: f64,
) -> io::Result<()> {
    if left.len() != right.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "stereo channel lengths differ",
        ));
    }
    let data_len = left
        .len()
        .checked_mul(8)
        .and_then(|length| u32::try_from(length).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "WAV is too large"))?;
    let mut file = fs::File::create(path)?;
    file.write_all(b"RIFF")?;
    file.write_all(&(36 + data_len).to_le_bytes())?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16u32.to_le_bytes())?;
    file.write_all(&3u16.to_le_bytes())?;
    file.write_all(&2u16.to_le_bytes())?;
    file.write_all(&sample_rate.to_le_bytes())?;
    file.write_all(&(sample_rate * 8).to_le_bytes())?;
    file.write_all(&8u16.to_le_bytes())?;
    file.write_all(&32u16.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&data_len.to_le_bytes())?;
    for (left, right) in left.iter().zip(right) {
        file.write_all(&((*left * gain) as f32).to_le_bytes())?;
        file.write_all(&((*right * gain) as f32).to_le_bytes())?;
    }
    Ok(())
}

struct ParsedDsf {
    left_msb: Vec<u8>,
    right_msb: Vec<u8>,
    wire_rate: u32,
    sample_count: usize,
}

fn parse_dsf(path: &Path) -> Result<ParsedDsf, String> {
    let bytes = fs::read(path).map_err(|error| format!("could not read DSF: {error}"))?;
    if bytes.len() < 92 || &bytes[..4] != b"DSD " || &bytes[28..32] != b"fmt " {
        return Err("external-product output is not a DSF with a leading fmt chunk".into());
    }
    let format_version = le_u32(&bytes, 40)?;
    let format_id = le_u32(&bytes, 44)?;
    let channels = le_u32(&bytes, 52)?;
    let wire_rate = le_u32(&bytes, 56)?;
    let bits_per_sample = le_u32(&bytes, 60)?;
    let sample_count =
        usize::try_from(le_u64(&bytes, 64)?).map_err(|_| "DSF sample count does not fit usize")?;
    let block_size =
        usize::try_from(le_u32(&bytes, 72)?).map_err(|_| "DSF block size does not fit usize")?;
    if format_version != 1
        || format_id != 0
        || channels != 2
        || bits_per_sample != 1
        || block_size == 0
    {
        return Err(format!(
            "unsupported DSF fmt: version={format_version} id={format_id} channels={channels} bits={bits_per_sample} block={block_size}"
        ));
    }
    let data_offset = 80usize;
    if &bytes[data_offset..data_offset + 4] != b"data" {
        return Err("DSF data chunk does not follow fmt chunk".into());
    }
    let data_chunk_size = usize::try_from(le_u64(&bytes, data_offset + 4)?)
        .map_err(|_| "DSF data chunk does not fit usize")?;
    if data_chunk_size < 12 || data_offset + data_chunk_size > bytes.len() {
        return Err("DSF data chunk is truncated".into());
    }
    let payload = &bytes[data_offset + 12..data_offset + data_chunk_size];
    let channel_bytes = sample_count.div_ceil(8);
    let mut left = Vec::with_capacity(channel_bytes);
    let mut right = Vec::with_capacity(channel_bytes);
    for pair in payload.chunks(block_size * 2) {
        if pair.len() < block_size * 2 {
            return Err("DSF ended with a partial stereo block pair".into());
        }
        left.extend(pair[..block_size].iter().map(|byte| byte.reverse_bits()));
        right.extend(
            pair[block_size..block_size * 2]
                .iter()
                .map(|byte| byte.reverse_bits()),
        );
    }
    if left.len() < channel_bytes || right.len() < channel_bytes {
        return Err("DSF payload is shorter than its declared sample count".into());
    }
    left.truncate(channel_bytes);
    right.truncate(channel_bytes);
    Ok(ParsedDsf {
        left_msb: left,
        right_msb: right,
        wire_rate,
        sample_count,
    })
}

fn le_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    bytes
        .get(offset..offset + 4)
        .and_then(|value| value.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| "truncated DSF u32".into())
}

fn le_u64(bytes: &[u8], offset: usize) -> Result<u64, String> {
    bytes
        .get(offset..offset + 8)
        .and_then(|value| value.try_into().ok())
        .map(u64::from_le_bytes)
        .ok_or_else(|| "truncated DSF u64".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_planar_lsb_first_dsf_into_msb_channels() {
        let path = std::env::temp_dir().join(format!(
            "fozmo-external-product-dsf-{}.dsf",
            std::process::id()
        ));
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"DSD ");
        bytes.extend_from_slice(&28u64.to_le_bytes());
        bytes.extend_from_slice(&100u64.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&52u64.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(&5_644_800u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&16u64.to_le_bytes());
        bytes.extend_from_slice(&4u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&20u64.to_le_bytes());
        bytes.extend_from_slice(&[0x01, 0x80, 0x00, 0x00]);
        bytes.extend_from_slice(&[0x02, 0x40, 0x00, 0x00]);
        fs::write(&path, bytes).unwrap();
        let parsed = parse_dsf(&path).unwrap();
        fs::remove_file(&path).unwrap();
        assert_eq!(parsed.wire_rate, 5_644_800);
        assert_eq!(parsed.sample_count, 16);
        assert_eq!(parsed.left_msb, [0x80, 0x01]);
        assert_eq!(parsed.right_msb, [0x40, 0x02]);
    }

    #[test]
    fn writes_stereo_ieee_float_wav() {
        let path = std::env::temp_dir().join(format!(
            "fozmo-external-product-wav-{}.wav",
            std::process::id()
        ));
        write_stereo_float_wav(&path, 44_100, &[0.5], &[-0.25], 0.5).unwrap();
        let bytes = fs::read(&path).unwrap();
        fs::remove_file(&path).unwrap();
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(u16::from_le_bytes(bytes[20..22].try_into().unwrap()), 3);
        assert_eq!(u16::from_le_bytes(bytes[22..24].try_into().unwrap()), 2);
        assert_eq!(f32::from_le_bytes(bytes[44..48].try_into().unwrap()), 0.25);
        assert_eq!(
            f32::from_le_bytes(bytes[48..52].try_into().unwrap()),
            -0.125
        );
    }
}
