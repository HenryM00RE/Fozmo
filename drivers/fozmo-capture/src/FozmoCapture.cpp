#include <CoreAudio/AudioHardware.h>
#include <CoreAudio/AudioServerPlugIn.h>
#include <CoreFoundation/CoreFoundation.h>
#include <dispatch/dispatch.h>
#include <mach/mach_time.h>
#include <time.h>

#include <atomic>
#include <cmath>
#include <cstddef>
#include <cstdint>
#include <cstring>

namespace {

constexpr AudioObjectID kObjectPlugIn = kAudioObjectPlugInObject;
constexpr AudioObjectID kObjectDevice = 2;
constexpr AudioObjectID kObjectOutputStream = 3;
constexpr AudioObjectID kObjectInputStream = 4;

constexpr UInt32 kChannels = 2;
constexpr UInt32 kBufferFrames = 512;
constexpr Float64 kDefaultSampleRate = 48000.0;
constexpr Float64 kSupportedRates[] = {44100.0, 48000.0, 88200.0, 96000.0, 176400.0, 192000.0};
constexpr UInt32 kSupportedRateCount = sizeof(kSupportedRates) / sizeof(kSupportedRates[0]);
constexpr UInt32 kRingFrames = 192000 * 4;
constexpr UInt32 kRingSamples = kRingFrames * kChannels;

// Bounded-latency input snap: when the input client falls this far behind the
// output writer, jump forward so live capture latency stays bounded instead of
// serving seconds of stale backlog.
constexpr UInt64 kSnapThresholdSamples = 8ULL * kBufferFrames * kChannels;
constexpr UInt64 kSnapBacklogSamples = 2ULL * kBufferFrames * kChannels;

// Keep in sync with CFBundleShortVersionString in Info.plist so users can
// prove which bundle coreaudiod actually loaded.
constexpr const char* kDriverVersion = "0.2.0";

constexpr AudioObjectPropertySelector kPropRingFillFrames = 'trff';
constexpr AudioObjectPropertySelector kPropRingFillMs = 'trfm';
constexpr AudioObjectPropertySelector kPropBufferFrames = 'trbf';
constexpr AudioObjectPropertySelector kPropUnderruns = 'trun';
constexpr AudioObjectPropertySelector kPropOverruns = 'trov';
constexpr AudioObjectPropertySelector kPropSnaps = 'trsn';
constexpr AudioObjectPropertySelector kPropLastRateChangeMs = 'trrc';
constexpr AudioObjectPropertySelector kPropLastStartMs = 'trst';
constexpr AudioObjectPropertySelector kPropLastStopMs = 'trsp';
constexpr AudioObjectPropertySelector kPropVersion = 'trvr';

AudioServerPlugInHostRef gHost = nullptr;
std::atomic<UInt32> gRefCount{1};
std::atomic<bool> gRunning{false};
std::atomic<UInt32> gRunningClients{0};
std::atomic<UInt64> gReadIndex{0};
std::atomic<UInt64> gWriteIndex{0};
std::atomic<UInt64> gUnderruns{0};
std::atomic<UInt64> gOverruns{0};
std::atomic<UInt64> gSnaps{0};
std::atomic<UInt64> gLastRateChangeMs{0};
std::atomic<UInt64> gLastStartMs{0};
std::atomic<UInt64> gLastStopMs{0};
std::atomic<UInt64> gAnchorHostTime{0};
std::atomic<UInt64> gClockSeed{1};
std::atomic<uint64_t> gSampleRateBits{0};
std::atomic<uint64_t> gHostTicksPerPeriodBits{0};

// Cached at Initialize; GetZeroTimeStamp must not call mach_timebase_info.
mach_timebase_info_data_t gTimebase = {0, 0};

alignas(64) float gRing[kRingSamples] = {};

Float64 sampleRate() {
    uint64_t bits = gSampleRateBits.load(std::memory_order_relaxed);
    if (bits == 0) {
        return kDefaultSampleRate;
    }
    Float64 value = 0.0;
    std::memcpy(&value, &bits, sizeof(value));
    return value;
}

void setSampleRate(Float64 value) {
    uint64_t bits = 0;
    std::memcpy(&bits, &value, sizeof(bits));
    gSampleRateBits.store(bits, std::memory_order_relaxed);
}

double hostTicksPerPeriod() {
    uint64_t bits = gHostTicksPerPeriodBits.load(std::memory_order_relaxed);
    double value = 0.0;
    std::memcpy(&value, &bits, sizeof(value));
    return value;
}

void updateHostTicksPerPeriod(Float64 rate) {
    double numer = gTimebase.numer == 0 ? 1.0 : static_cast<double>(gTimebase.numer);
    double denom = gTimebase.denom == 0 ? 1.0 : static_cast<double>(gTimebase.denom);
    // host ticks = nanos * denom / numer; one period is kBufferFrames frames.
    double ticks = (static_cast<double>(kBufferFrames) / rate) * 1e9 * denom / numer;
    uint64_t bits = 0;
    std::memcpy(&bits, &ticks, sizeof(bits));
    gHostTicksPerPeriodBits.store(bits, std::memory_order_relaxed);
}

// Unix-epoch wall clock read from the commpage; safe on the IO path, unlike
// CFAbsoluteTimeGetCurrent (a CF call whose epoch is 2001, not 1970 — the app
// treats these values as unix ms).
UInt64 wallMs() {
    return clock_gettime_nsec_np(CLOCK_REALTIME) / 1000000ULL;
}

bool isSupportedRate(Float64 rate) {
    for (Float64 supported : kSupportedRates) {
        if (std::fabs(supported - rate) < 0.5) {
            return true;
        }
    }
    return false;
}

AudioStreamBasicDescription asbdForRate(Float64 rate) {
    AudioStreamBasicDescription format = {};
    format.mSampleRate = rate;
    format.mFormatID = kAudioFormatLinearPCM;
    format.mFormatFlags = kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked;
    format.mBytesPerPacket = sizeof(float) * kChannels;
    format.mFramesPerPacket = 1;
    format.mBytesPerFrame = sizeof(float) * kChannels;
    format.mChannelsPerFrame = kChannels;
    format.mBitsPerChannel = sizeof(float) * 8;
    return format;
}

AudioStreamBasicDescription streamFormat() {
    return asbdForRate(sampleRate());
}

// A rate-only change request is valid when everything except mSampleRate
// matches the fixed F32/stereo layout this device exposes.
bool isRateOnlyFormat(const AudioStreamBasicDescription& asbd) {
    const AudioStreamBasicDescription expected = asbdForRate(asbd.mSampleRate);
    return asbd.mFormatID == expected.mFormatID &&
           (asbd.mFormatFlags & kAudioFormatFlagIsFloat) != 0 &&
           asbd.mBytesPerPacket == expected.mBytesPerPacket &&
           asbd.mFramesPerPacket == expected.mFramesPerPacket &&
           asbd.mBytesPerFrame == expected.mBytesPerFrame &&
           asbd.mChannelsPerFrame == expected.mChannelsPerFrame &&
           asbd.mBitsPerChannel == expected.mBitsPerChannel;
}

UInt64 ringFillSamples() {
    UInt64 write = gWriteIndex.load(std::memory_order_acquire);
    UInt64 read = gReadIndex.load(std::memory_order_acquire);
    UInt64 fill = write >= read ? write - read : 0;
    return fill > kRingSamples ? kRingSamples : fill;
}

void ringReset() {
    gReadIndex.store(0, std::memory_order_release);
    gWriteIndex.store(0, std::memory_order_release);
}

// Indices are monotonic sample counters; the ring offset is index % kRingSamples.
// Transfers are at most two contiguous memcpy slices.
void ringWrite(const float* input, UInt32 frames) {
    if (input == nullptr || frames == 0) {
        return;
    }
    UInt64 sampleCount = static_cast<UInt64>(frames) * kChannels;
    if (sampleCount > kRingSamples) {
        input += sampleCount - kRingSamples;
        sampleCount = kRingSamples;
    }
    UInt64 read = gReadIndex.load(std::memory_order_acquire);
    UInt64 write = gWriteIndex.load(std::memory_order_relaxed);
    UInt64 fill = write >= read ? write - read : 0;
    if (fill > kRingSamples) {
        fill = kRingSamples;
        read = write - kRingSamples;
        gReadIndex.store(read, std::memory_order_release);
    }
    if (sampleCount > kRingSamples - fill) {
        // Drop-oldest: advance the reader past the samples about to be overwritten.
        const UInt64 drop = sampleCount - (kRingSamples - fill);
        gReadIndex.store(read + drop, std::memory_order_release);
        gOverruns.fetch_add(1, std::memory_order_relaxed);
    }
    const UInt64 offset = write % kRingSamples;
    const UInt64 first = sampleCount < kRingSamples - offset ? sampleCount : kRingSamples - offset;
    std::memcpy(gRing + offset, input, static_cast<size_t>(first) * sizeof(float));
    if (first < sampleCount) {
        std::memcpy(gRing, input + first, static_cast<size_t>(sampleCount - first) * sizeof(float));
    }
    gWriteIndex.store(write + sampleCount, std::memory_order_release);
}

void ringRead(float* output, UInt32 frames) {
    if (output == nullptr || frames == 0) {
        return;
    }
    const UInt64 sampleCount = static_cast<UInt64>(frames) * kChannels;
    UInt64 read = gReadIndex.load(std::memory_order_relaxed);
    UInt64 write = gWriteIndex.load(std::memory_order_acquire);
    UInt64 available = write >= read ? write - read : 0;
    if (available > kRingSamples) {
        available = kRingSamples;
        read = write - kRingSamples;
    }
    if (available > kSnapThresholdSamples) {
        read = write - kSnapBacklogSamples;
        available = kSnapBacklogSamples;
        gSnaps.fetch_add(1, std::memory_order_relaxed);
    }
    const UInt64 toRead = sampleCount < available ? sampleCount : available;
    const UInt64 offset = read % kRingSamples;
    const UInt64 first = toRead < kRingSamples - offset ? toRead : kRingSamples - offset;
    std::memcpy(output, gRing + offset, static_cast<size_t>(first) * sizeof(float));
    if (first < toRead) {
        std::memcpy(output + first, gRing, static_cast<size_t>(toRead - first) * sizeof(float));
    }
    if (toRead < sampleCount) {
        std::memset(output + toRead, 0, static_cast<size_t>(sampleCount - toRead) * sizeof(float));
        gUnderruns.fetch_add(1, std::memory_order_relaxed);
    }
    gReadIndex.store(read + toRead, std::memory_order_release);
}

// Nominal-rate changes go through the CoreAudio host configuration-change
// handshake: SetPropertyData validates and requests, and the host calls
// PerformDeviceConfigurationChange, the only place a rate is applied.
void requestRateChange(void* context) {
    if (gHost != nullptr) {
        gHost->RequestDeviceConfigurationChange(
            gHost, kObjectDevice, static_cast<UInt64>(reinterpret_cast<uintptr_t>(context)), nullptr);
    }
}

void notifyRateProperties(void*) {
    if (gHost == nullptr) {
        return;
    }
    const AudioObjectPropertyAddress deviceAddress = {
        kAudioDevicePropertyNominalSampleRate,
        kAudioObjectPropertyScopeGlobal,
        kAudioObjectPropertyElementMain,
    };
    gHost->PropertiesChanged(gHost, kObjectDevice, 1, &deviceAddress);
    const AudioObjectPropertyAddress streamAddresses[] = {
        {kAudioStreamPropertyVirtualFormat, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain},
        {kAudioStreamPropertyPhysicalFormat, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain},
    };
    gHost->PropertiesChanged(gHost, kObjectOutputStream, 2, streamAddresses);
    gHost->PropertiesChanged(gHost, kObjectInputStream, 2, streamAddresses);
}

void dispatchRateChangeRequest(Float64 rate) {
    const uintptr_t action = static_cast<uintptr_t>(llround(rate));
    dispatch_async_f(dispatch_get_global_queue(QOS_CLASS_DEFAULT, 0),
                     reinterpret_cast<void*>(action), requestRateChange);
}

void copyASBD(void* outData, UInt32 inDataSize, UInt32* outDataSize) {
    if (inDataSize >= sizeof(AudioStreamBasicDescription)) {
        *reinterpret_cast<AudioStreamBasicDescription*>(outData) = streamFormat();
        *outDataSize = sizeof(AudioStreamBasicDescription);
    }
}

CFStringRef makeString(const char* value) {
    return CFStringCreateWithCString(kCFAllocatorDefault, value, kCFStringEncodingUTF8);
}

template <typename T>
OSStatus writeScalar(UInt32 inDataSize, UInt32* outDataSize, void* outData, T value) {
    if (inDataSize < sizeof(T)) {
        return kAudioHardwareBadPropertySizeError;
    }
    *reinterpret_cast<T*>(outData) = value;
    *outDataSize = sizeof(T);
    return kAudioHardwareNoError;
}

OSStatus writeCFString(UInt32 inDataSize, UInt32* outDataSize, void* outData, const char* value) {
    if (inDataSize < sizeof(CFStringRef)) {
        return kAudioHardwareBadPropertySizeError;
    }
    *reinterpret_cast<CFStringRef*>(outData) = makeString(value);
    *outDataSize = sizeof(CFStringRef);
    return kAudioHardwareNoError;
}

UInt32 audioBufferListSize(UInt32 bufferCount) {
    return static_cast<UInt32>(offsetof(AudioBufferList, mBuffers) + (sizeof(AudioBuffer) * bufferCount));
}

OSStatus writeStreamConfiguration(const AudioObjectPropertyAddress* address,
                                  UInt32 inDataSize,
                                  UInt32* outDataSize,
                                  void* outData) {
    UInt32 bufferCount = address->mScope == kAudioObjectPropertyScopeGlobal ? 2 : 1;
    UInt32 requiredSize = audioBufferListSize(bufferCount);
    if (inDataSize < requiredSize) {
        return kAudioHardwareBadPropertySizeError;
    }

    auto* bufferList = reinterpret_cast<AudioBufferList*>(outData);
    bufferList->mNumberBuffers = bufferCount;
    for (UInt32 i = 0; i < bufferCount; ++i) {
        bufferList->mBuffers[i].mNumberChannels = kChannels;
        bufferList->mBuffers[i].mDataByteSize = 0;
        bufferList->mBuffers[i].mData = nullptr;
    }
    *outDataSize = requiredSize;
    return kAudioHardwareNoError;
}

bool objectExists(AudioObjectID objectID) {
    return objectID == kObjectPlugIn || objectID == kObjectDevice || objectID == kObjectOutputStream ||
           objectID == kObjectInputStream;
}

Boolean driverHasProperty(AudioObjectID objectID, const AudioObjectPropertyAddress* address) {
    if (!objectExists(objectID) || address == nullptr) {
        return false;
    }
    switch (objectID) {
        case kObjectPlugIn:
            switch (address->mSelector) {
                case kAudioObjectPropertyBaseClass:
                case kAudioObjectPropertyClass:
                case kAudioObjectPropertyOwner:
                case kAudioObjectPropertyName:
                case kAudioObjectPropertyManufacturer:
                case kAudioObjectPropertyOwnedObjects:
                case kAudioPlugInPropertyBundleID:
                case kAudioPlugInPropertyDeviceList:
                case kAudioPlugInPropertyTranslateUIDToDevice:
                    return true;
            }
            break;
        case kObjectDevice:
            switch (address->mSelector) {
                case kAudioObjectPropertyBaseClass:
                case kAudioObjectPropertyClass:
                case kAudioObjectPropertyOwner:
                case kAudioObjectPropertyName:
                case kAudioObjectPropertyManufacturer:
                case kAudioObjectPropertyOwnedObjects:
                case kAudioObjectPropertyCustomPropertyInfoList:
                case kAudioDevicePropertyDeviceUID:
                case kAudioDevicePropertyModelUID:
                case kAudioDevicePropertyTransportType:
                case kAudioDevicePropertyRelatedDevices:
                case kAudioDevicePropertyClockDomain:
                case kAudioDevicePropertyDeviceIsAlive:
                case kAudioDevicePropertyDeviceIsRunning:
                case kAudioDevicePropertyDeviceCanBeDefaultDevice:
                case kAudioDevicePropertyDeviceCanBeDefaultSystemDevice:
                case kAudioDevicePropertyLatency:
                case kAudioDevicePropertyStreams:
                case kAudioDevicePropertyStreamConfiguration:
                case kAudioDevicePropertyZeroTimeStampPeriod:
                case kAudioDevicePropertySafetyOffset:
                case kAudioDevicePropertyNominalSampleRate:
                case kAudioDevicePropertyAvailableNominalSampleRates:
                case kAudioDevicePropertyIsHidden:
                case kAudioDevicePropertyPreferredChannelsForStereo:
                case kPropRingFillFrames:
                case kPropRingFillMs:
                case kPropBufferFrames:
                case kPropUnderruns:
                case kPropOverruns:
                case kPropSnaps:
                case kPropLastRateChangeMs:
                case kPropLastStartMs:
                case kPropLastStopMs:
                case kPropVersion:
                    return true;
            }
            break;
        case kObjectOutputStream:
        case kObjectInputStream:
            switch (address->mSelector) {
                case kAudioObjectPropertyBaseClass:
                case kAudioObjectPropertyClass:
                case kAudioObjectPropertyOwner:
                case kAudioObjectPropertyName:
                case kAudioStreamPropertyIsActive:
                case kAudioStreamPropertyDirection:
                case kAudioStreamPropertyTerminalType:
                case kAudioStreamPropertyStartingChannel:
                case kAudioStreamPropertyLatency:
                case kAudioStreamPropertyVirtualFormat:
                case kAudioStreamPropertyAvailableVirtualFormats:
                case kAudioStreamPropertyPhysicalFormat:
                case kAudioStreamPropertyAvailablePhysicalFormats:
                    return true;
            }
            break;
    }
    return false;
}

UInt32 propertyDataSize(AudioObjectID objectID, const AudioObjectPropertyAddress* address, UInt32 qualifierSize) {
    switch (address->mSelector) {
        case kAudioObjectPropertyName:
        case kAudioObjectPropertyManufacturer:
        case kAudioPlugInPropertyBundleID:
        case kAudioDevicePropertyDeviceUID:
        case kAudioDevicePropertyModelUID:
        case kPropVersion:
            return sizeof(CFStringRef);
        case kAudioObjectPropertyBaseClass:
        case kAudioObjectPropertyClass:
        case kAudioObjectPropertyOwner:
        case kAudioDevicePropertyTransportType:
        case kAudioDevicePropertyClockDomain:
        case kAudioDevicePropertyDeviceIsAlive:
        case kAudioDevicePropertyDeviceIsRunning:
        case kAudioDevicePropertyDeviceCanBeDefaultDevice:
        case kAudioDevicePropertyDeviceCanBeDefaultSystemDevice:
        case kAudioDevicePropertyLatency:
        case kAudioDevicePropertyZeroTimeStampPeriod:
        case kAudioDevicePropertySafetyOffset:
        case kAudioDevicePropertyIsHidden:
        case kAudioStreamPropertyIsActive:
        case kAudioStreamPropertyDirection:
        case kAudioStreamPropertyTerminalType:
        case kAudioStreamPropertyStartingChannel:
        // kAudioStreamPropertyLatency shares the 'ltnc' selector with
        // kAudioDevicePropertyLatency above.
        case kPropBufferFrames:
            return sizeof(UInt32);
        case kAudioDevicePropertyNominalSampleRate:
        case kPropRingFillMs:
            return sizeof(Float64);
        case kPropRingFillFrames:
        case kPropUnderruns:
        case kPropOverruns:
        case kPropSnaps:
        case kPropLastRateChangeMs:
        case kPropLastStartMs:
        case kPropLastStopMs:
            return sizeof(UInt64);
        case kAudioStreamPropertyVirtualFormat:
        case kAudioStreamPropertyPhysicalFormat:
            return sizeof(AudioStreamBasicDescription);
        case kAudioDevicePropertyAvailableNominalSampleRates:
            return sizeof(AudioValueRange) * kSupportedRateCount;
        case kAudioStreamPropertyAvailableVirtualFormats:
        case kAudioStreamPropertyAvailablePhysicalFormats:
            return sizeof(AudioStreamRangedDescription) * kSupportedRateCount;
        case kAudioObjectPropertyCustomPropertyInfoList:
            return sizeof(AudioServerPlugInCustomPropertyInfo);
        case kAudioDevicePropertyPreferredChannelsForStereo:
            return sizeof(UInt32) * 2;
        case kAudioObjectPropertyOwnedObjects:
            if (objectID == kObjectPlugIn) {
                return sizeof(AudioObjectID);
            }
            if (objectID == kObjectDevice) {
                return sizeof(AudioObjectID) * 2;
            }
            return 0;
        case kAudioDevicePropertyStreams:
        case kAudioDevicePropertyStreamConfiguration:
            if (address->mScope == kAudioObjectPropertyScopeInput ||
                address->mScope == kAudioObjectPropertyScopeOutput) {
                return address->mSelector == kAudioDevicePropertyStreams ? sizeof(AudioObjectID)
                                                                         : audioBufferListSize(1);
            }
            return address->mSelector == kAudioDevicePropertyStreams ? sizeof(AudioObjectID) * 2
                                                                     : audioBufferListSize(2);
        case kAudioDevicePropertyRelatedDevices:
            return sizeof(AudioObjectID);
        case kAudioPlugInPropertyDeviceList:
            return sizeof(AudioObjectID);
        case kAudioPlugInPropertyTranslateUIDToDevice:
            return qualifierSize >= sizeof(CFStringRef) ? sizeof(AudioObjectID) : 0;
        default:
            return 0;
    }
}

OSStatus getPropertyData(AudioObjectID objectID,
                         const AudioObjectPropertyAddress* address,
                         UInt32 qualifierSize,
                         const void* qualifierData,
                         UInt32 inDataSize,
                         UInt32* outDataSize,
                         void* outData) {
    if (!driverHasProperty(objectID, address)) {
        return kAudioHardwareUnknownPropertyError;
    }
    *outDataSize = 0;

    switch (address->mSelector) {
        case kAudioObjectPropertyBaseClass:
            return writeScalar(inDataSize, outDataSize, outData, static_cast<AudioClassID>(kAudioObjectClassID));
        case kAudioObjectPropertyClass:
            if (objectID == kObjectPlugIn) {
                return writeScalar(inDataSize, outDataSize, outData, static_cast<AudioClassID>(kAudioPlugInClassID));
            }
            if (objectID == kObjectDevice) {
                return writeScalar(inDataSize, outDataSize, outData, static_cast<AudioClassID>(kAudioDeviceClassID));
            }
            return writeScalar(inDataSize, outDataSize, outData, static_cast<AudioClassID>(kAudioStreamClassID));
        case kAudioObjectPropertyOwner:
            if (objectID == kObjectPlugIn) {
                return writeScalar(inDataSize, outDataSize, outData, static_cast<AudioObjectID>(0));
            }
            if (objectID == kObjectDevice) {
                return writeScalar(inDataSize, outDataSize, outData, kObjectPlugIn);
            }
            return writeScalar(inDataSize, outDataSize, outData, kObjectDevice);
        case kAudioObjectPropertyName:
            if (objectID == kObjectOutputStream) {
                return writeCFString(inDataSize, outDataSize, outData, "Fozmo Capture Output");
            }
            if (objectID == kObjectInputStream) {
                return writeCFString(inDataSize, outDataSize, outData, "Fozmo Capture Input");
            }
            return writeCFString(inDataSize, outDataSize, outData, "Fozmo Capture");
        case kAudioObjectPropertyManufacturer:
            return writeCFString(inDataSize, outDataSize, outData, "Fozmo");
        case kAudioPlugInPropertyBundleID:
            return writeCFString(inDataSize, outDataSize, outData, "com.fozmo.audio.capture.driver");
        case kAudioPlugInPropertyDeviceList:
        case kAudioObjectPropertyOwnedObjects:
            if (objectID == kObjectPlugIn) {
                if (inDataSize < sizeof(AudioObjectID)) return kAudioHardwareBadPropertySizeError;
                reinterpret_cast<AudioObjectID*>(outData)[0] = kObjectDevice;
                *outDataSize = sizeof(AudioObjectID);
                return kAudioHardwareNoError;
            }
            if (objectID == kObjectDevice) {
                if (inDataSize < sizeof(AudioObjectID) * 2) return kAudioHardwareBadPropertySizeError;
                reinterpret_cast<AudioObjectID*>(outData)[0] = kObjectOutputStream;
                reinterpret_cast<AudioObjectID*>(outData)[1] = kObjectInputStream;
                *outDataSize = sizeof(AudioObjectID) * 2;
                return kAudioHardwareNoError;
            }
            return kAudioHardwareUnknownPropertyError;
        case kAudioObjectPropertyCustomPropertyInfoList: {
            // Only the CFString version property is declared as a custom
            // property; the scalar telemetry selectors stay readable by known
            // selector for the app but are not advertised (UInt64 is not a
            // supported custom-property data type).
            if (inDataSize < sizeof(AudioServerPlugInCustomPropertyInfo)) {
                return kAudioHardwareBadPropertySizeError;
            }
            auto* info = reinterpret_cast<AudioServerPlugInCustomPropertyInfo*>(outData);
            info->mSelector = kPropVersion;
            info->mPropertyDataType = kAudioServerPlugInCustomPropertyDataTypeCFString;
            info->mQualifierDataType = kAudioServerPlugInCustomPropertyDataTypeNone;
            *outDataSize = sizeof(AudioServerPlugInCustomPropertyInfo);
            return kAudioHardwareNoError;
        }
        case kAudioPlugInPropertyTranslateUIDToDevice:
            if (qualifierSize >= sizeof(CFStringRef) && qualifierData != nullptr) {
                CFStringRef uid = *reinterpret_cast<CFStringRef const*>(qualifierData);
                if (uid != nullptr && CFStringCompare(uid, CFSTR("com.fozmo.audio.capture"), 0) == kCFCompareEqualTo) {
                    return writeScalar(inDataSize, outDataSize, outData, kObjectDevice);
                }
            }
            return kAudioHardwareBadObjectError;
        case kAudioDevicePropertyDeviceUID:
            return writeCFString(inDataSize, outDataSize, outData, "com.fozmo.audio.capture");
        case kAudioDevicePropertyModelUID:
            return writeCFString(inDataSize, outDataSize, outData, "com.fozmo.audio.capture.model");
        case kAudioDevicePropertyTransportType:
            return writeScalar(inDataSize, outDataSize, outData, static_cast<UInt32>(kAudioDeviceTransportTypeVirtual));
        case kAudioDevicePropertyRelatedDevices:
            if (inDataSize < sizeof(AudioObjectID)) return kAudioHardwareBadPropertySizeError;
            reinterpret_cast<AudioObjectID*>(outData)[0] = kObjectDevice;
            *outDataSize = sizeof(AudioObjectID);
            return kAudioHardwareNoError;
        case kAudioDevicePropertyClockDomain:
        case kAudioDevicePropertyLatency:
        case kAudioDevicePropertySafetyOffset:
        case kAudioDevicePropertyIsHidden:
            // Latency and SafetyOffset stay 0: this device is a bit-transparent
            // capture path, not real hardware.
            return writeScalar(inDataSize, outDataSize, outData, static_cast<UInt32>(0));
        case kAudioDevicePropertyZeroTimeStampPeriod:
            return writeScalar(inDataSize, outDataSize, outData, kBufferFrames);
        case kPropBufferFrames:
            return writeScalar(inDataSize, outDataSize, outData, kBufferFrames);
        case kAudioDevicePropertyDeviceIsAlive:
        case kAudioDevicePropertyDeviceCanBeDefaultDevice:
        case kAudioDevicePropertyDeviceCanBeDefaultSystemDevice:
        case kAudioStreamPropertyIsActive:
            return writeScalar(inDataSize, outDataSize, outData, static_cast<UInt32>(1));
        case kAudioDevicePropertyDeviceIsRunning:
            return writeScalar(inDataSize, outDataSize, outData, static_cast<UInt32>(gRunning.load(std::memory_order_relaxed) ? 1 : 0));
        case kAudioDevicePropertyStreams:
            if (address->mScope == kAudioObjectPropertyScopeInput) {
                if (inDataSize < sizeof(AudioObjectID)) return kAudioHardwareBadPropertySizeError;
                reinterpret_cast<AudioObjectID*>(outData)[0] = kObjectInputStream;
                *outDataSize = sizeof(AudioObjectID);
                return kAudioHardwareNoError;
            }
            if (address->mScope == kAudioObjectPropertyScopeOutput) {
                if (inDataSize < sizeof(AudioObjectID)) return kAudioHardwareBadPropertySizeError;
                reinterpret_cast<AudioObjectID*>(outData)[0] = kObjectOutputStream;
                *outDataSize = sizeof(AudioObjectID);
                return kAudioHardwareNoError;
            }
            if (inDataSize < sizeof(AudioObjectID) * 2) return kAudioHardwareBadPropertySizeError;
            reinterpret_cast<AudioObjectID*>(outData)[0] = kObjectOutputStream;
            reinterpret_cast<AudioObjectID*>(outData)[1] = kObjectInputStream;
            *outDataSize = sizeof(AudioObjectID) * 2;
            return kAudioHardwareNoError;
        case kAudioDevicePropertyStreamConfiguration:
            return writeStreamConfiguration(address, inDataSize, outDataSize, outData);
        case kAudioDevicePropertyNominalSampleRate:
            return writeScalar(inDataSize, outDataSize, outData, sampleRate());
        case kAudioDevicePropertyAvailableNominalSampleRates: {
            if (inDataSize < sizeof(AudioValueRange) * kSupportedRateCount) return kAudioHardwareBadPropertySizeError;
            auto* ranges = reinterpret_cast<AudioValueRange*>(outData);
            for (UInt32 i = 0; i < kSupportedRateCount; ++i) {
                ranges[i].mMinimum = kSupportedRates[i];
                ranges[i].mMaximum = kSupportedRates[i];
            }
            *outDataSize = sizeof(AudioValueRange) * kSupportedRateCount;
            return kAudioHardwareNoError;
        }
        case kAudioDevicePropertyPreferredChannelsForStereo:
            if (inDataSize < sizeof(UInt32) * 2) return kAudioHardwareBadPropertySizeError;
            reinterpret_cast<UInt32*>(outData)[0] = 1;
            reinterpret_cast<UInt32*>(outData)[1] = 2;
            *outDataSize = sizeof(UInt32) * 2;
            return kAudioHardwareNoError;
        case kAudioStreamPropertyDirection:
            return writeScalar(inDataSize, outDataSize, outData,
                               static_cast<UInt32>(objectID == kObjectInputStream ? 1 : 0));
        case kAudioStreamPropertyTerminalType:
            return writeScalar(inDataSize, outDataSize, outData,
                               static_cast<UInt32>(objectID == kObjectInputStream
                                                       ? kAudioStreamTerminalTypeMicrophone
                                                       : kAudioStreamTerminalTypeSpeaker));
        case kAudioStreamPropertyStartingChannel:
            return writeScalar(inDataSize, outDataSize, outData, static_cast<UInt32>(1));
        case kAudioStreamPropertyVirtualFormat:
        case kAudioStreamPropertyPhysicalFormat:
            copyASBD(outData, inDataSize, outDataSize);
            return *outDataSize == sizeof(AudioStreamBasicDescription) ? kAudioHardwareNoError
                                                                       : kAudioHardwareBadPropertySizeError;
        case kAudioStreamPropertyAvailableVirtualFormats:
        case kAudioStreamPropertyAvailablePhysicalFormats: {
            if (inDataSize < sizeof(AudioStreamRangedDescription) * kSupportedRateCount) {
                return kAudioHardwareBadPropertySizeError;
            }
            auto* descriptions = reinterpret_cast<AudioStreamRangedDescription*>(outData);
            for (UInt32 i = 0; i < kSupportedRateCount; ++i) {
                descriptions[i].mFormat = asbdForRate(kSupportedRates[i]);
                descriptions[i].mSampleRateRange.mMinimum = kSupportedRates[i];
                descriptions[i].mSampleRateRange.mMaximum = kSupportedRates[i];
            }
            *outDataSize = sizeof(AudioStreamRangedDescription) * kSupportedRateCount;
            return kAudioHardwareNoError;
        }
        case kPropRingFillFrames:
            return writeScalar(inDataSize, outDataSize, outData, ringFillSamples() / kChannels);
        case kPropRingFillMs: {
            Float64 ms = (static_cast<Float64>(ringFillSamples() / kChannels) / sampleRate()) * 1000.0;
            return writeScalar(inDataSize, outDataSize, outData, ms);
        }
        case kPropUnderruns:
            return writeScalar(inDataSize, outDataSize, outData, gUnderruns.load(std::memory_order_relaxed));
        case kPropOverruns:
            return writeScalar(inDataSize, outDataSize, outData, gOverruns.load(std::memory_order_relaxed));
        case kPropSnaps:
            return writeScalar(inDataSize, outDataSize, outData, gSnaps.load(std::memory_order_relaxed));
        case kPropLastRateChangeMs:
            return writeScalar(inDataSize, outDataSize, outData, gLastRateChangeMs.load(std::memory_order_relaxed));
        case kPropLastStartMs:
            return writeScalar(inDataSize, outDataSize, outData, gLastStartMs.load(std::memory_order_relaxed));
        case kPropLastStopMs:
            return writeScalar(inDataSize, outDataSize, outData, gLastStopMs.load(std::memory_order_relaxed));
        case kPropVersion:
            return writeCFString(inDataSize, outDataSize, outData, kDriverVersion);
    }
    return kAudioHardwareUnknownPropertyError;
}

OSStatus setPropertyData(AudioObjectID objectID,
                         const AudioObjectPropertyAddress* address,
                         UInt32 inDataSize,
                         const void* inData) {
    if (objectID == kObjectDevice && address->mSelector == kAudioDevicePropertyNominalSampleRate) {
        if (inDataSize < sizeof(Float64)) return kAudioHardwareBadPropertySizeError;
        Float64 next = *reinterpret_cast<const Float64*>(inData);
        if (!isSupportedRate(next)) return kAudioHardwareIllegalOperationError;
        if (std::fabs(next - sampleRate()) < 0.5) return kAudioHardwareNoError;
        dispatchRateChangeRequest(next);
        return kAudioHardwareNoError;
    }
    if ((objectID == kObjectOutputStream || objectID == kObjectInputStream) &&
        (address->mSelector == kAudioStreamPropertyVirtualFormat ||
         address->mSelector == kAudioStreamPropertyPhysicalFormat)) {
        if (inDataSize < sizeof(AudioStreamBasicDescription)) return kAudioHardwareBadPropertySizeError;
        const auto* asbd = reinterpret_cast<const AudioStreamBasicDescription*>(inData);
        if (!isRateOnlyFormat(*asbd)) return kAudioDeviceUnsupportedFormatError;
        if (!isSupportedRate(asbd->mSampleRate)) return kAudioDeviceUnsupportedFormatError;
        if (std::fabs(asbd->mSampleRate - sampleRate()) < 0.5) return kAudioHardwareNoError;
        dispatchRateChangeRequest(asbd->mSampleRate);
        return kAudioHardwareNoError;
    }
    return kAudioHardwareUnknownPropertyError;
}

HRESULT STDMETHODCALLTYPE QueryInterface(void* inDriver, REFIID inUUID, LPVOID* outInterface);
ULONG STDMETHODCALLTYPE AddRef(void* inDriver);
ULONG STDMETHODCALLTYPE Release(void* inDriver);

OSStatus STDMETHODCALLTYPE Initialize(AudioServerPlugInDriverRef inDriver, AudioServerPlugInHostRef inHost);
OSStatus STDMETHODCALLTYPE CreateDevice(AudioServerPlugInDriverRef, CFDictionaryRef, const AudioServerPlugInClientInfo*, AudioObjectID*);
OSStatus STDMETHODCALLTYPE DestroyDevice(AudioServerPlugInDriverRef, AudioObjectID);
OSStatus STDMETHODCALLTYPE AddDeviceClient(AudioServerPlugInDriverRef, AudioObjectID, const AudioServerPlugInClientInfo*);
OSStatus STDMETHODCALLTYPE RemoveDeviceClient(AudioServerPlugInDriverRef, AudioObjectID, const AudioServerPlugInClientInfo*);
OSStatus STDMETHODCALLTYPE PerformDeviceConfigurationChange(AudioServerPlugInDriverRef, AudioObjectID, UInt64, void*);
OSStatus STDMETHODCALLTYPE AbortDeviceConfigurationChange(AudioServerPlugInDriverRef, AudioObjectID, UInt64, void*);
Boolean STDMETHODCALLTYPE HasProperty(AudioServerPlugInDriverRef, AudioObjectID, pid_t, const AudioObjectPropertyAddress*);
OSStatus STDMETHODCALLTYPE IsPropertySettable(AudioServerPlugInDriverRef, AudioObjectID, pid_t, const AudioObjectPropertyAddress*, Boolean*);
OSStatus STDMETHODCALLTYPE GetPropertyDataSize(AudioServerPlugInDriverRef, AudioObjectID, pid_t, const AudioObjectPropertyAddress*, UInt32, const void*, UInt32*);
OSStatus STDMETHODCALLTYPE GetPropertyData(AudioServerPlugInDriverRef, AudioObjectID, pid_t, const AudioObjectPropertyAddress*, UInt32, const void*, UInt32, UInt32*, void*);
OSStatus STDMETHODCALLTYPE SetPropertyData(AudioServerPlugInDriverRef, AudioObjectID, pid_t, const AudioObjectPropertyAddress*, UInt32, const void*, UInt32, const void*);
OSStatus STDMETHODCALLTYPE StartIO(AudioServerPlugInDriverRef, AudioObjectID, UInt32);
OSStatus STDMETHODCALLTYPE StopIO(AudioServerPlugInDriverRef, AudioObjectID, UInt32);
OSStatus STDMETHODCALLTYPE GetZeroTimeStamp(AudioServerPlugInDriverRef, AudioObjectID, UInt32, Float64*, UInt64*, UInt64*);
OSStatus STDMETHODCALLTYPE WillDoIOOperation(AudioServerPlugInDriverRef, AudioObjectID, UInt32, UInt32, Boolean*, Boolean*);
OSStatus STDMETHODCALLTYPE BeginIOOperation(AudioServerPlugInDriverRef, AudioObjectID, UInt32, UInt32, UInt32, const AudioServerPlugInIOCycleInfo*);
OSStatus STDMETHODCALLTYPE DoIOOperation(AudioServerPlugInDriverRef, AudioObjectID, AudioObjectID, UInt32, UInt32, UInt32, const AudioServerPlugInIOCycleInfo*, void*, void*);
OSStatus STDMETHODCALLTYPE EndIOOperation(AudioServerPlugInDriverRef, AudioObjectID, UInt32, UInt32, UInt32, const AudioServerPlugInIOCycleInfo*);

AudioServerPlugInDriverInterface gInterface = {
    nullptr,
    QueryInterface,
    AddRef,
    Release,
    Initialize,
    CreateDevice,
    DestroyDevice,
    AddDeviceClient,
    RemoveDeviceClient,
    PerformDeviceConfigurationChange,
    AbortDeviceConfigurationChange,
    HasProperty,
    IsPropertySettable,
    GetPropertyDataSize,
    GetPropertyData,
    SetPropertyData,
    StartIO,
    StopIO,
    GetZeroTimeStamp,
    WillDoIOOperation,
    BeginIOOperation,
    DoIOOperation,
    EndIOOperation,
};

AudioServerPlugInDriverInterface* gInterfacePtr = &gInterface;

HRESULT STDMETHODCALLTYPE QueryInterface(void*, REFIID inUUID, LPVOID* outInterface) {
    if (outInterface == nullptr) {
        return E_POINTER;
    }
    CFUUIDRef requested = CFUUIDCreateFromUUIDBytes(kCFAllocatorDefault, inUUID);
    if (CFEqual(requested, IUnknownUUID) || CFEqual(requested, kAudioServerPlugInDriverInterfaceUUID)) {
        AddRef(&gInterfacePtr);
        *outInterface = &gInterfacePtr;
        CFRelease(requested);
        return S_OK;
    }
    *outInterface = nullptr;
    CFRelease(requested);
    return E_NOINTERFACE;
}

ULONG STDMETHODCALLTYPE AddRef(void*) {
    return gRefCount.fetch_add(1, std::memory_order_relaxed) + 1;
}

ULONG STDMETHODCALLTYPE Release(void*) {
    UInt32 next = gRefCount.fetch_sub(1, std::memory_order_relaxed) - 1;
    return next;
}

OSStatus STDMETHODCALLTYPE Initialize(AudioServerPlugInDriverRef, AudioServerPlugInHostRef inHost) {
    gHost = inHost;
    mach_timebase_info(&gTimebase);
    setSampleRate(kDefaultSampleRate);
    updateHostTicksPerPeriod(kDefaultSampleRate);
    gAnchorHostTime.store(mach_absolute_time(), std::memory_order_relaxed);
    ringReset();
    gUnderruns.store(0, std::memory_order_relaxed);
    gOverruns.store(0, std::memory_order_relaxed);
    gSnaps.store(0, std::memory_order_relaxed);
    return kAudioHardwareNoError;
}

OSStatus STDMETHODCALLTYPE CreateDevice(AudioServerPlugInDriverRef, CFDictionaryRef, const AudioServerPlugInClientInfo*, AudioObjectID* outDeviceObjectID) {
    if (outDeviceObjectID != nullptr) {
        *outDeviceObjectID = kObjectDevice;
    }
    return kAudioHardwareNoError;
}

OSStatus STDMETHODCALLTYPE DestroyDevice(AudioServerPlugInDriverRef, AudioObjectID) {
    return kAudioHardwareNoError;
}

OSStatus STDMETHODCALLTYPE AddDeviceClient(AudioServerPlugInDriverRef, AudioObjectID, const AudioServerPlugInClientInfo*) {
    return kAudioHardwareNoError;
}

OSStatus STDMETHODCALLTYPE RemoveDeviceClient(AudioServerPlugInDriverRef, AudioObjectID, const AudioServerPlugInClientInfo*) {
    return kAudioHardwareNoError;
}

OSStatus STDMETHODCALLTYPE PerformDeviceConfigurationChange(AudioServerPlugInDriverRef,
                                                            AudioObjectID inDeviceObjectID,
                                                            UInt64 inChangeAction,
                                                            void*) {
    if (inDeviceObjectID != kObjectDevice) {
        return kAudioHardwareBadObjectError;
    }
    const Float64 next = static_cast<Float64>(inChangeAction);
    if (!isSupportedRate(next)) {
        return kAudioHardwareIllegalOperationError;
    }
    setSampleRate(next);
    updateHostTicksPerPeriod(next);
    ringReset();
    gAnchorHostTime.store(mach_absolute_time(), std::memory_order_relaxed);
    gClockSeed.fetch_add(1, std::memory_order_relaxed);
    gLastRateChangeMs.store(wallMs(), std::memory_order_relaxed);
    dispatch_async_f(dispatch_get_global_queue(QOS_CLASS_DEFAULT, 0), nullptr, notifyRateProperties);
    return kAudioHardwareNoError;
}

OSStatus STDMETHODCALLTYPE AbortDeviceConfigurationChange(AudioServerPlugInDriverRef, AudioObjectID, UInt64, void*) {
    return kAudioHardwareNoError;
}

Boolean STDMETHODCALLTYPE HasProperty(AudioServerPlugInDriverRef, AudioObjectID objectID, pid_t, const AudioObjectPropertyAddress* address) {
    return driverHasProperty(objectID, address);
}

OSStatus STDMETHODCALLTYPE IsPropertySettable(AudioServerPlugInDriverRef, AudioObjectID objectID, pid_t, const AudioObjectPropertyAddress* address, Boolean* outIsSettable) {
    if (outIsSettable == nullptr) return kAudioHardwareIllegalOperationError;
    const bool deviceRate = objectID == kObjectDevice && address != nullptr &&
                            address->mSelector == kAudioDevicePropertyNominalSampleRate;
    const bool streamFormatSelector =
        (objectID == kObjectOutputStream || objectID == kObjectInputStream) && address != nullptr &&
        (address->mSelector == kAudioStreamPropertyVirtualFormat ||
         address->mSelector == kAudioStreamPropertyPhysicalFormat);
    *outIsSettable = deviceRate || streamFormatSelector;
    return driverHasProperty(objectID, address) ? kAudioHardwareNoError : kAudioHardwareUnknownPropertyError;
}

OSStatus STDMETHODCALLTYPE GetPropertyDataSize(AudioServerPlugInDriverRef, AudioObjectID objectID, pid_t, const AudioObjectPropertyAddress* address, UInt32 qualifierSize, const void*, UInt32* outDataSize) {
    if (outDataSize == nullptr || address == nullptr) return kAudioHardwareIllegalOperationError;
    if (!driverHasProperty(objectID, address)) return kAudioHardwareUnknownPropertyError;
    *outDataSize = propertyDataSize(objectID, address, qualifierSize);
    return *outDataSize == 0 ? kAudioHardwareUnknownPropertyError : kAudioHardwareNoError;
}

OSStatus STDMETHODCALLTYPE GetPropertyData(AudioServerPlugInDriverRef,
                                           AudioObjectID objectID,
                                           pid_t,
                                           const AudioObjectPropertyAddress* address,
                                           UInt32 qualifierSize,
                                           const void* qualifierData,
                                           UInt32 inDataSize,
                                           UInt32* outDataSize,
                                           void* outData) {
    if (address == nullptr || outDataSize == nullptr || outData == nullptr) return kAudioHardwareIllegalOperationError;
    return getPropertyData(objectID, address, qualifierSize, qualifierData, inDataSize, outDataSize, outData);
}

OSStatus STDMETHODCALLTYPE SetPropertyData(AudioServerPlugInDriverRef,
                                           AudioObjectID objectID,
                                           pid_t,
                                           const AudioObjectPropertyAddress* address,
                                           UInt32,
                                           const void*,
                                           UInt32 inDataSize,
                                           const void* inData) {
    if (address == nullptr || inData == nullptr) return kAudioHardwareIllegalOperationError;
    return setPropertyData(objectID, address, inDataSize, inData);
}

OSStatus STDMETHODCALLTYPE StartIO(AudioServerPlugInDriverRef, AudioObjectID, UInt32) {
    const UInt32 previous = gRunningClients.fetch_add(1, std::memory_order_acq_rel);
    if (previous == 0) {
        // First running client: drop any stale backlog and anchor the clock.
        // Later clients must not smash the timeline of already-running ones.
        ringReset();
        gAnchorHostTime.store(mach_absolute_time(), std::memory_order_relaxed);
        gRunning.store(true, std::memory_order_release);
        gLastStartMs.store(wallMs(), std::memory_order_relaxed);
    }
    return kAudioHardwareNoError;
}

OSStatus STDMETHODCALLTYPE StopIO(AudioServerPlugInDriverRef, AudioObjectID, UInt32) {
    UInt32 clients = gRunningClients.load(std::memory_order_relaxed);
    while (clients > 0 && !gRunningClients.compare_exchange_weak(clients, clients - 1)) {}
    if (clients == 1) {
        gRunning.store(false, std::memory_order_release);
        gLastStopMs.store(wallMs(), std::memory_order_relaxed);
    }
    return kAudioHardwareNoError;
}

OSStatus STDMETHODCALLTYPE GetZeroTimeStamp(AudioServerPlugInDriverRef, AudioObjectID, UInt32, Float64* outSampleTime, UInt64* outHostTime, UInt64* outSeed) {
    if (outSampleTime == nullptr || outHostTime == nullptr || outSeed == nullptr) return kAudioHardwareIllegalOperationError;
    // NullAudio-style quantized timestamps: advance in whole kBufferFrames
    // periods from the anchor instead of deriving continuous wall-clock time.
    const UInt64 hostNow = mach_absolute_time();
    const UInt64 anchor = gAnchorHostTime.load(std::memory_order_relaxed);
    const double ticksPerPeriod = hostTicksPerPeriod();
    const UInt64 elapsed = hostNow >= anchor ? hostNow - anchor : 0;
    const UInt64 count = ticksPerPeriod > 0.0
                             ? static_cast<UInt64>(static_cast<double>(elapsed) / ticksPerPeriod)
                             : 0;
    *outSampleTime = static_cast<Float64>(count) * static_cast<Float64>(kBufferFrames);
    *outHostTime = anchor + static_cast<UInt64>(static_cast<double>(count) * ticksPerPeriod);
    *outSeed = gClockSeed.load(std::memory_order_relaxed);
    return kAudioHardwareNoError;
}

OSStatus STDMETHODCALLTYPE WillDoIOOperation(AudioServerPlugInDriverRef,
                                             AudioObjectID,
                                             UInt32,
                                             UInt32 operationID,
                                             Boolean* outWillDo,
                                             Boolean* outWillDoInPlace) {
    if (outWillDo == nullptr || outWillDoInPlace == nullptr) return kAudioHardwareIllegalOperationError;
    bool willDo = operationID == kAudioServerPlugInIOOperationReadInput ||
                  operationID == kAudioServerPlugInIOOperationWriteMix;
    *outWillDo = willDo;
    *outWillDoInPlace = true;
    return kAudioHardwareNoError;
}

OSStatus STDMETHODCALLTYPE BeginIOOperation(AudioServerPlugInDriverRef, AudioObjectID, UInt32, UInt32, UInt32, const AudioServerPlugInIOCycleInfo*) {
    return kAudioHardwareNoError;
}

OSStatus STDMETHODCALLTYPE DoIOOperation(AudioServerPlugInDriverRef,
                                         AudioObjectID,
                                         AudioObjectID streamID,
                                         UInt32,
                                         UInt32 operationID,
                                         UInt32 frameSize,
                                         const AudioServerPlugInIOCycleInfo*,
                                         void* ioMainBuffer,
                                         void*) {
    if (streamID == kObjectOutputStream && operationID == kAudioServerPlugInIOOperationWriteMix) {
        ringWrite(reinterpret_cast<const float*>(ioMainBuffer), frameSize);
        return kAudioHardwareNoError;
    }
    if (streamID == kObjectInputStream && operationID == kAudioServerPlugInIOOperationReadInput) {
        ringRead(reinterpret_cast<float*>(ioMainBuffer), frameSize);
        return kAudioHardwareNoError;
    }
    return kAudioHardwareNoError;
}

OSStatus STDMETHODCALLTYPE EndIOOperation(AudioServerPlugInDriverRef, AudioObjectID, UInt32, UInt32, UInt32, const AudioServerPlugInIOCycleInfo*) {
    return kAudioHardwareNoError;
}

}  // namespace

extern "C" __attribute__((visibility("default"))) void* FozmoCaptureFactory(CFAllocatorRef, CFUUIDRef typeUUID) {
    if (typeUUID == nullptr || CFEqual(typeUUID, kAudioServerPlugInTypeUUID)) {
        AddRef(&gInterfacePtr);
        return &gInterfacePtr;
    }
    return nullptr;
}
