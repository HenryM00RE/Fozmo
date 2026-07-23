#import <AppKit/AppKit.h>
#import <CoreAudio/AudioHardware.h>
#import <CoreAudio/AudioHardwareTapping.h>
#import <CoreAudio/CATapDescription.h>
#import <Foundation/Foundation.h>
#import <stdatomic.h>
#import <stdint.h>
#import <stdlib.h>
#import <string.h>

#pragma clang diagnostic ignored "-Wunguarded-availability-new"

typedef void (*FozmoProcessTapAudioCallback)(
    const float *buffer0,
    const float *buffer1,
    uint32_t frames,
    uint32_t layout,
    uint64_t host_time,
    void *context
);

typedef struct {
    int32_t pid;
    uint32_t process_object_id;
    uint32_t tap_object_id;
    uint32_t aggregate_device_id;
    double sample_rate_hz;
    uint32_t channels;
    uint32_t format_flags;
    uint32_t interleaved;
    uint32_t bits_per_channel;
    uint32_t bytes_per_frame;
    uint32_t format_settable_known;
    uint32_t format_settable;
} FozmoProcessTapInfo;

typedef struct {
    AudioObjectID process_object_id;
    AudioObjectID tap_object_id;
    AudioObjectID aggregate_device_id;
    AudioDeviceIOProcID io_proc_id;
    AudioStreamBasicDescription format;
    FozmoProcessTapAudioCallback callback;
    void *callback_context;
    _Atomic(bool) started;
} FozmoProcessTapHandle;

enum {
    FOZMO_TAP_STAGE_NONE = 0,
    FOZMO_TAP_STAGE_OS_SUPPORT = 1,
    FOZMO_TAP_STAGE_AUDIO_PROCESS = 2,
    FOZMO_TAP_STAGE_CREATE_TAP = 3,
    FOZMO_TAP_STAGE_TAP_FORMAT = 4,
    FOZMO_TAP_STAGE_TAP_UID = 5,
    FOZMO_TAP_STAGE_CREATE_AGGREGATE = 6,
    FOZMO_TAP_STAGE_ATTACH_TAP = 7,
    FOZMO_TAP_STAGE_CREATE_IO_PROC = 8,
    FOZMO_TAP_STAGE_START_IO = 9,
};

enum {
    FOZMO_STATUS_UNSUPPORTED = 0x6f733f3f, /* os?? */
    FOZMO_STATUS_PROCESS_NOT_AUDIO = 0x7072633f, /* prc? */
    FOZMO_STATUS_UNSUPPORTED_FORMAT = 0x666d743f, /* fmt? */
};

static AudioObjectPropertyAddress kGlobalMainAddress(
    AudioObjectPropertySelector selector
) {
    const AudioObjectPropertyAddress address = {
        selector,
        kAudioObjectPropertyScopeGlobal,
        kAudioObjectPropertyElementMain,
    };
    return address;
}

static void set_failure(
    int32_t status,
    uint32_t stage,
    int32_t *out_status,
    uint32_t *out_stage
) {
    if (out_status != NULL) {
        *out_status = status;
    }
    if (out_stage != NULL) {
        *out_stage = stage;
    }
}

static AudioObjectID audio_process_for_pid(pid_t pid, OSStatus *out_status) {
    AudioObjectID process_id = kAudioObjectUnknown;
    UInt32 size = sizeof(process_id);
    const AudioObjectPropertyAddress address =
        kGlobalMainAddress(kAudioHardwarePropertyTranslatePIDToProcessObject);
    const OSStatus status = AudioObjectGetPropertyData(
        kAudioObjectSystemObject,
        &address,
        sizeof(pid),
        &pid,
        &size,
        &process_id
    );
    if (out_status != NULL) {
        *out_status = status;
    }
    return process_id;
}

static OSStatus copy_default_system_output_uid(
    AudioObjectID *out_device_id,
    CFStringRef *out_uid
) {
    if (out_device_id == NULL || out_uid == NULL) {
        return paramErr;
    }
    *out_device_id = kAudioObjectUnknown;
    *out_uid = NULL;

    UInt32 property_size = sizeof(*out_device_id);
    AudioObjectPropertyAddress property_address =
        kGlobalMainAddress(kAudioHardwarePropertyDefaultSystemOutputDevice);
    OSStatus status = AudioObjectGetPropertyData(
        kAudioObjectSystemObject,
        &property_address,
        0,
        NULL,
        &property_size,
        out_device_id
    );
    if (status != kAudioHardwareNoError
        || *out_device_id == kAudioObjectUnknown) {
        return status == kAudioHardwareNoError ? kAudioHardwareBadDeviceError : status;
    }

    property_size = sizeof(*out_uid);
    property_address = kGlobalMainAddress(kAudioDevicePropertyDeviceUID);
    status = AudioObjectGetPropertyData(
        *out_device_id,
        &property_address,
        0,
        NULL,
        &property_size,
        out_uid
    );
    if (status != kAudioHardwareNoError || *out_uid == NULL) {
        if (*out_uid != NULL) {
            CFRelease(*out_uid);
            *out_uid = NULL;
        }
        return status == kAudioHardwareNoError ? kAudioHardwareBadDeviceError : status;
    }
    return kAudioHardwareNoError;
}

static OSStatus process_tap_io_proc(
    AudioObjectID device_id,
    const AudioTimeStamp *now,
    const AudioBufferList *input,
    const AudioTimeStamp *input_time,
    AudioBufferList *output,
    const AudioTimeStamp *output_time,
    void *client_data
) {
    (void)device_id;
    (void)output_time;

    if (output != NULL) {
        for (UInt32 index = 0; index < output->mNumberBuffers; index += 1) {
            AudioBuffer *buffer = &output->mBuffers[index];
            if (buffer->mData != NULL && buffer->mDataByteSize > 0) {
                memset(buffer->mData, 0, buffer->mDataByteSize);
            }
        }
    }

    FozmoProcessTapHandle *handle = client_data;
    if (handle == NULL || handle->callback == NULL || input == NULL
        || input->mNumberBuffers == 0) {
        return kAudioHardwareNoError;
    }

    uint64_t host_time = 0;
    if (input_time != NULL) {
        host_time = input_time->mHostTime;
    } else if (now != NULL) {
        host_time = now->mHostTime;
    }

    const bool non_interleaved =
        (handle->format.mFormatFlags & kAudioFormatFlagIsNonInterleaved) != 0;
    if (!non_interleaved) {
        const AudioBuffer buffer = input->mBuffers[0];
        if (buffer.mData == NULL || buffer.mNumberChannels < 2) {
            return kAudioHardwareNoError;
        }
        const UInt32 bytes_per_frame =
            buffer.mNumberChannels * (UInt32)sizeof(Float32);
        const UInt32 frames =
            bytes_per_frame == 0 ? 0 : buffer.mDataByteSize / bytes_per_frame;
        if (frames > 0) {
            handle->callback(
                (const float *)buffer.mData,
                NULL,
                frames,
                0,
                host_time,
                handle->callback_context
            );
        }
        return kAudioHardwareNoError;
    }

    if (input->mNumberBuffers < 2) {
        return kAudioHardwareNoError;
    }
    const AudioBuffer left = input->mBuffers[0];
    const AudioBuffer right = input->mBuffers[1];
    if (left.mData == NULL || right.mData == NULL) {
        return kAudioHardwareNoError;
    }
    const UInt32 left_frames = left.mDataByteSize / (UInt32)sizeof(Float32);
    const UInt32 right_frames = right.mDataByteSize / (UInt32)sizeof(Float32);
    const UInt32 frames = left_frames < right_frames ? left_frames : right_frames;
    if (frames > 0) {
        handle->callback(
            (const float *)left.mData,
            (const float *)right.mData,
            frames,
            1,
            host_time,
            handle->callback_context
        );
    }
    return kAudioHardwareNoError;
}

static void destroy_handle(FozmoProcessTapHandle *handle) {
    if (handle == NULL) {
        return;
    }
    if (atomic_exchange_explicit(&handle->started, false, memory_order_acq_rel)) {
        AudioDeviceStop(handle->aggregate_device_id, handle->io_proc_id);
    }
    handle->callback = NULL;
    handle->callback_context = NULL;
    if (handle->io_proc_id != NULL && handle->aggregate_device_id != kAudioObjectUnknown) {
        AudioDeviceDestroyIOProcID(handle->aggregate_device_id, handle->io_proc_id);
        handle->io_proc_id = NULL;
    }
    if (handle->aggregate_device_id != kAudioObjectUnknown) {
        AudioHardwareDestroyAggregateDevice(handle->aggregate_device_id);
        handle->aggregate_device_id = kAudioObjectUnknown;
    }
    if (handle->tap_object_id != kAudioObjectUnknown) {
        AudioHardwareDestroyProcessTap(handle->tap_object_id);
        handle->tap_object_id = kAudioObjectUnknown;
    }
    free(handle);
}

uint32_t fozmo_process_tap_supported(void) {
    const NSOperatingSystemVersion minimum = {
        .majorVersion = 14,
        .minorVersion = 2,
        .patchVersion = 0,
    };
    return [NSProcessInfo.processInfo isOperatingSystemAtLeastVersion:minimum] ? 1 : 0;
}

int32_t fozmo_music_app_pid(void) {
    @autoreleasepool {
        NSArray<NSRunningApplication *> *applications =
            [NSRunningApplication runningApplicationsWithBundleIdentifier:@"com.apple.Music"];
        for (NSRunningApplication *application in applications) {
            if (!application.terminated && application.processIdentifier > 0) {
                return application.processIdentifier;
            }
        }
        return 0;
    }
}

void *fozmo_process_tap_create(
    int32_t pid,
    uint32_t mute_original,
    FozmoProcessTapInfo *out_info,
    int32_t *out_status,
    uint32_t *out_stage
) {
    set_failure(kAudioHardwareNoError, FOZMO_TAP_STAGE_NONE, out_status, out_stage);
    if (out_info != NULL) {
        *out_info = (FozmoProcessTapInfo){0};
    }
    if (!fozmo_process_tap_supported()) {
        set_failure(
            FOZMO_STATUS_UNSUPPORTED,
            FOZMO_TAP_STAGE_OS_SUPPORT,
            out_status,
            out_stage
        );
        return NULL;
    }

    FozmoProcessTapHandle *handle = calloc(1, sizeof(FozmoProcessTapHandle));
    if (handle == NULL) {
        set_failure(memFullErr, FOZMO_TAP_STAGE_CREATE_TAP, out_status, out_stage);
        return NULL;
    }
    handle->process_object_id = kAudioObjectUnknown;
    handle->tap_object_id = kAudioObjectUnknown;
    handle->aggregate_device_id = kAudioObjectUnknown;
    atomic_init(&handle->started, false);

    @autoreleasepool {
        OSStatus status = kAudioHardwareNoError;
        handle->process_object_id = audio_process_for_pid((pid_t)pid, &status);
        if (status != kAudioHardwareNoError
            || handle->process_object_id == kAudioObjectUnknown) {
            set_failure(
                status == kAudioHardwareNoError ? FOZMO_STATUS_PROCESS_NOT_AUDIO : status,
                FOZMO_TAP_STAGE_AUDIO_PROCESS,
                out_status,
                out_stage
            );
            destroy_handle(handle);
            return NULL;
        }

        CATapDescription *description = [[CATapDescription alloc]
            initStereoMixdownOfProcesses:@[@(handle->process_object_id)]];
        description.name = @"Fozmo Music app DSP experiment";
        description.privateTap = YES;
        description.exclusive = NO;
        description.mixdown = YES;
        description.mono = NO;
        description.muteBehavior =
            mute_original ? CATapMutedWhenTapped : CATapUnmuted;

        status = AudioHardwareCreateProcessTap(description, &handle->tap_object_id);
        if (status != kAudioHardwareNoError) {
            set_failure(status, FOZMO_TAP_STAGE_CREATE_TAP, out_status, out_stage);
            destroy_handle(handle);
            return NULL;
        }

        UInt32 property_size = sizeof(handle->format);
        AudioObjectPropertyAddress property_address =
            kGlobalMainAddress(kAudioTapPropertyFormat);
        status = AudioObjectGetPropertyData(
            handle->tap_object_id,
            &property_address,
            0,
            NULL,
            &property_size,
            &handle->format
        );
        const bool is_float32_stereo =
            status == kAudioHardwareNoError
            && handle->format.mFormatID == kAudioFormatLinearPCM
            && (handle->format.mFormatFlags & kAudioFormatFlagIsFloat) != 0
            && handle->format.mBitsPerChannel == 32
            && handle->format.mChannelsPerFrame == 2
            && handle->format.mSampleRate > 0;
        if (!is_float32_stereo) {
            set_failure(
                status == kAudioHardwareNoError ? FOZMO_STATUS_UNSUPPORTED_FORMAT : status,
                FOZMO_TAP_STAGE_TAP_FORMAT,
                out_status,
                out_stage
            );
            destroy_handle(handle);
            return NULL;
        }

        Boolean format_settable = false;
        const OSStatus format_settable_status = AudioObjectIsPropertySettable(
            handle->tap_object_id,
            &property_address,
            &format_settable
        );

        CFStringRef tap_uid = NULL;
        property_size = sizeof(tap_uid);
        property_address = kGlobalMainAddress(kAudioTapPropertyUID);
        status = AudioObjectGetPropertyData(
            handle->tap_object_id,
            &property_address,
            0,
            NULL,
            &property_size,
            &tap_uid
        );
        if (status != kAudioHardwareNoError || tap_uid == NULL) {
            set_failure(status, FOZMO_TAP_STAGE_TAP_UID, out_status, out_stage);
            if (tap_uid != NULL) {
                CFRelease(tap_uid);
            }
            destroy_handle(handle);
            return NULL;
        }

        AudioObjectID default_output_id = kAudioObjectUnknown;
        CFStringRef default_output_uid = NULL;
        status = copy_default_system_output_uid(
            &default_output_id,
            &default_output_uid
        );
        if (status != kAudioHardwareNoError || default_output_uid == NULL) {
            set_failure(
                status,
                FOZMO_TAP_STAGE_CREATE_AGGREGATE,
                out_status,
                out_stage
            );
            CFRelease(tap_uid);
            destroy_handle(handle);
            return NULL;
        }

        NSString *aggregate_uid = [NSString stringWithFormat:
            @"com.fozmo.music-process-tap.%@", NSUUID.UUID.UUIDString];
        NSDictionary *subdevice_description = @{
            @kAudioSubDeviceUIDKey: (__bridge NSString *)default_output_uid,
        };
        NSDictionary *subtap_description = @{
            @kAudioSubTapUIDKey: (__bridge NSString *)tap_uid,
            @kAudioSubTapDriftCompensationKey: @YES,
        };
        NSDictionary *aggregate_description = @{
            @kAudioAggregateDeviceNameKey: @"Fozmo Music app process tap",
            @kAudioAggregateDeviceUIDKey: aggregate_uid,
            @kAudioAggregateDeviceMainSubDeviceKey:
                (__bridge NSString *)default_output_uid,
            @kAudioAggregateDeviceIsPrivateKey: @YES,
            @kAudioAggregateDeviceIsStackedKey: @NO,
            @kAudioAggregateDeviceTapAutoStartKey: @YES,
            @kAudioAggregateDeviceSubDeviceListKey: @[subdevice_description],
            @kAudioAggregateDeviceTapListKey: @[subtap_description],
        };
        status = AudioHardwareCreateAggregateDevice(
            (__bridge CFDictionaryRef)aggregate_description,
            &handle->aggregate_device_id
        );
        CFRelease(default_output_uid);
        CFRelease(tap_uid);
        if (status != kAudioHardwareNoError) {
            set_failure(
                status,
                FOZMO_TAP_STAGE_CREATE_AGGREGATE,
                out_status,
                out_stage
            );
            destroy_handle(handle);
            return NULL;
        }

        status = AudioDeviceCreateIOProcID(
            handle->aggregate_device_id,
            process_tap_io_proc,
            handle,
            &handle->io_proc_id
        );
        if (status != kAudioHardwareNoError) {
            set_failure(
                status,
                FOZMO_TAP_STAGE_CREATE_IO_PROC,
                out_status,
                out_stage
            );
            destroy_handle(handle);
            return NULL;
        }

        if (out_info != NULL) {
            *out_info = (FozmoProcessTapInfo){
                .pid = pid,
                .process_object_id = handle->process_object_id,
                .tap_object_id = handle->tap_object_id,
                .aggregate_device_id = handle->aggregate_device_id,
                .sample_rate_hz = handle->format.mSampleRate,
                .channels = handle->format.mChannelsPerFrame,
                .format_flags = handle->format.mFormatFlags,
                .interleaved =
                    (handle->format.mFormatFlags & kAudioFormatFlagIsNonInterleaved)
                    == 0,
                .bits_per_channel = handle->format.mBitsPerChannel,
                .bytes_per_frame = handle->format.mBytesPerFrame,
                .format_settable_known =
                    format_settable_status == kAudioHardwareNoError,
                .format_settable =
                    format_settable_status == kAudioHardwareNoError
                    && format_settable,
            };
        }
    }
    return handle;
}

int32_t fozmo_process_tap_start(
    void *opaque_handle,
    FozmoProcessTapAudioCallback callback,
    void *callback_context,
    uint32_t *out_stage
) {
    FozmoProcessTapHandle *handle = opaque_handle;
    if (out_stage != NULL) {
        *out_stage = FOZMO_TAP_STAGE_START_IO;
    }
    if (handle == NULL || callback == NULL) {
        return paramErr;
    }
    handle->callback_context = callback_context;
    handle->callback = callback;
    const OSStatus status =
        AudioDeviceStart(handle->aggregate_device_id, handle->io_proc_id);
    if (status != kAudioHardwareNoError) {
        handle->callback = NULL;
        handle->callback_context = NULL;
        return status;
    }
    atomic_store_explicit(&handle->started, true, memory_order_release);
    return kAudioHardwareNoError;
}

void fozmo_process_tap_stop(void *opaque_handle) {
    destroy_handle((FozmoProcessTapHandle *)opaque_handle);
}
