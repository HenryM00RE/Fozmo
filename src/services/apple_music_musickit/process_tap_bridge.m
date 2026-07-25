#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
#import <CoreAudio/AudioHardware.h>
#import <CoreAudio/AudioHardwareTapping.h>
#import <CoreAudio/CATapDescription.h>
#import <Foundation/Foundation.h>
#import <stdatomic.h>
#import <stdint.h>
#import <stdio.h>
#import <stdlib.h>
#import <string.h>
#import <time.h>
#import <unistd.h>

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
    FOZMO_STATUS_FORMAT_NOT_SETTABLE = 0x6673743f, /* fst? */
    FOZMO_STATUS_FORMAT_RATE_MISMATCH = 0x72617465, /* rate */
};

uint32_t fozmo_process_tap_supported(void);

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

int32_t fozmo_active_musickit_renderer_pids(
    int32_t *out_pids,
    uint32_t capacity
) {
    if (!fozmo_process_tap_supported()) {
        return 0;
    }

    AudioObjectPropertyAddress list_address =
        kGlobalMainAddress(kAudioHardwarePropertyProcessObjectList);
    UInt32 list_size = 0;
    OSStatus status = AudioObjectGetPropertyDataSize(
        kAudioObjectSystemObject,
        &list_address,
        0,
        NULL,
        &list_size
    );
    if (status != kAudioHardwareNoError) {
        return -1;
    }
    if (list_size == 0) {
        return 0;
    }

    AudioObjectID *processes = malloc(list_size);
    if (processes == NULL) {
        return -1;
    }
    status = AudioObjectGetPropertyData(
        kAudioObjectSystemObject,
        &list_address,
        0,
        NULL,
        &list_size,
        processes
    );
    if (status != kAudioHardwareNoError) {
        free(processes);
        return -1;
    }

    int32_t renderer_count = 0;
    const UInt32 count = list_size / sizeof(AudioObjectID);
    for (UInt32 index = 0; index < count; index += 1) {
        const AudioObjectID process = processes[index];
        CFStringRef bundle_id = NULL;
        UInt32 property_size = sizeof(bundle_id);
        AudioObjectPropertyAddress property_address =
            kGlobalMainAddress(kAudioProcessPropertyBundleID);
        status = AudioObjectGetPropertyData(
            process,
            &property_address,
            0,
            NULL,
            &property_size,
            &bundle_id
        );
        const bool is_musickit_renderer =
            status == kAudioHardwareNoError
            && bundle_id != NULL
            && CFEqual(
                bundle_id,
                CFSTR("com.apple.MediaPlayer.RemotePlayerService")
            );
        if (bundle_id != NULL) {
            CFRelease(bundle_id);
        }
        if (!is_musickit_renderer) {
            continue;
        }

        UInt32 running_output = 0;
        property_size = sizeof(running_output);
        property_address =
            kGlobalMainAddress(kAudioProcessPropertyIsRunningOutput);
        status = AudioObjectGetPropertyData(
            process,
            &property_address,
            0,
            NULL,
            &property_size,
            &running_output
        );
        if (status != kAudioHardwareNoError || running_output == 0) {
            continue;
        }

        pid_t pid = 0;
        property_size = sizeof(pid);
        property_address = kGlobalMainAddress(kAudioProcessPropertyPID);
        status = AudioObjectGetPropertyData(
            process,
            &property_address,
            0,
            NULL,
            &property_size,
            &pid
        );
        if (status != kAudioHardwareNoError || pid <= 0) {
            continue;
        }
        if (out_pids != NULL && (uint32_t)renderer_count < capacity) {
            out_pids[renderer_count] = (int32_t)pid;
        }
        renderer_count += 1;
    }
    free(processes);
    return renderer_count;
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

static void fozmo_catalog_error(
    char *error_buffer,
    size_t error_capacity,
    NSString *message
) {
    if (error_buffer == NULL || error_capacity == 0) {
        return;
    }
    const char *utf8 = message.UTF8String;
    snprintf(
        error_buffer,
        error_capacity,
        "%s",
        utf8 != NULL ? utf8 : "Unknown Music.app automation error."
    );
}

static AXUIElementRef fozmo_find_accessibility_identifier_prefix(
    AXUIElementRef application,
    CFStringRef target_identifier_prefix
) {
    NSMutableArray *queue = [NSMutableArray arrayWithObject:(__bridge id)application];
    NSUInteger cursor = 0;
    const NSUInteger maximum_nodes = 50000;
    while (cursor < queue.count && cursor < maximum_nodes) {
        AXUIElementRef element = (__bridge AXUIElementRef)queue[cursor++];
        CFTypeRef identifier_value = NULL;
        if (AXUIElementCopyAttributeValue(
                element,
                kAXIdentifierAttribute,
                &identifier_value
            ) == kAXErrorSuccess
            && identifier_value != NULL) {
            const bool matches =
                CFGetTypeID(identifier_value) == CFStringGetTypeID()
                && CFStringHasPrefix(
                    (CFStringRef)identifier_value,
                    target_identifier_prefix
                );
            CFRelease(identifier_value);
            if (matches) {
                return (AXUIElementRef)CFRetain(element);
            }
        }

        CFTypeRef children_value = NULL;
        if (AXUIElementCopyAttributeValue(
                element,
                kAXChildrenAttribute,
                &children_value
            ) != kAXErrorSuccess
            || children_value == NULL) {
            continue;
        }
        if (CFGetTypeID(children_value) == CFArrayGetTypeID()) {
            NSArray *children = (__bridge NSArray *)children_value;
            [queue addObjectsFromArray:children];
        }
        CFRelease(children_value);
    }
    return NULL;
}

static bool fozmo_accessibility_bounds(
    AXUIElementRef element,
    CGPoint *position,
    CGSize *size
) {
    CFTypeRef position_value = NULL;
    CFTypeRef size_value = NULL;
    const AXError position_error = AXUIElementCopyAttributeValue(
        element,
        kAXPositionAttribute,
        &position_value
    );
    const AXError size_error = AXUIElementCopyAttributeValue(
        element,
        kAXSizeAttribute,
        &size_value
    );
    const bool valid =
        position_error == kAXErrorSuccess
        && size_error == kAXErrorSuccess
        && position_value != NULL
        && size_value != NULL
        && CFGetTypeID(position_value) == AXValueGetTypeID()
        && CFGetTypeID(size_value) == AXValueGetTypeID()
        && AXValueGetValue((AXValueRef)position_value, kAXValueCGPointType, position)
        && AXValueGetValue((AXValueRef)size_value, kAXValueCGSizeType, size);
    if (position_value != NULL) {
        CFRelease(position_value);
    }
    if (size_value != NULL) {
        CFRelease(size_value);
    }
    return valid;
}

static bool fozmo_post_double_click(CGPoint point) {
    CGEventSourceRef source =
        CGEventSourceCreate(kCGEventSourceStateHIDSystemState);
    if (source == NULL) {
        return false;
    }
    CGEventSourceSetLocalEventsSuppressionInterval(source, 0.0);

    CGEventRef location_event = CGEventCreate(source);
    if (location_event == NULL) {
        CFRelease(source);
        return false;
    }
    const CGPoint original_point = CGEventGetLocation(location_event);
    CFRelease(location_event);

    CGEventRef move_to_target = CGEventCreateMouseEvent(
        source,
        kCGEventMouseMoved,
        point,
        kCGMouseButtonLeft
    );
    if (move_to_target == NULL) {
        CFRelease(source);
        return false;
    }
    CGEventPost(kCGHIDEventTap, move_to_target);
    CFRelease(move_to_target);
    usleep(30000);

    bool posted = true;
    for (int64_t click_state = 1; click_state <= 2; click_state += 1) {
        CGEventRef down = CGEventCreateMouseEvent(
            source,
            kCGEventLeftMouseDown,
            point,
            kCGMouseButtonLeft
        );
        CGEventRef up = CGEventCreateMouseEvent(
            source,
            kCGEventLeftMouseUp,
            point,
            kCGMouseButtonLeft
        );
        if (down == NULL || up == NULL) {
            if (down != NULL) {
                CFRelease(down);
            }
            if (up != NULL) {
                CFRelease(up);
            }
            posted = false;
            break;
        }

        // The click-state sequence is what AppKit uses to recognize a double
        // click. Do not synthesize kCGMouseEventNumber: Music's web-backed
        // album rows reject a pair whose two clicks have different event IDs.
        CGEventSetIntegerValueField(
            down,
            kCGMouseEventClickState,
            click_state
        );
        CGEventSetIntegerValueField(
            up,
            kCGMouseEventClickState,
            click_state
        );
        CGEventPost(kCGHIDEventTap, down);
        CGEventPost(kCGHIDEventTap, up);
        CFRelease(down);
        CFRelease(up);
        usleep(90000);
    }

    CGEventRef move_to_original = CGEventCreateMouseEvent(
        source,
        kCGEventMouseMoved,
        original_point,
        kCGMouseButtonLeft
    );
    if (move_to_original != NULL) {
        CGEventPost(kCGHIDEventTap, move_to_original);
        CFRelease(move_to_original);
    }
    CFRelease(source);
    return posted;
}

static void fozmo_restore_frontmost(NSRunningApplication *previous_frontmost) {
    if (previous_frontmost != nil
        && ![previous_frontmost.bundleIdentifier isEqualToString:@"com.apple.Music"]) {
        [previous_frontmost activateWithOptions:NSApplicationActivateIgnoringOtherApps];
    }
}

int32_t fozmo_music_activate_catalog_track(
    const char *storefront,
    const char *album_id,
    const char *song_id,
    char *error_buffer,
    size_t error_capacity
) {
    @autoreleasepool {
        if (error_buffer != NULL && error_capacity > 0) {
            error_buffer[0] = '\0';
        }
        NSDictionary *trust_options = @{
            (__bridge NSString *)kAXTrustedCheckOptionPrompt: @YES
        };
        if (!AXIsProcessTrustedWithOptions(
                (__bridge CFDictionaryRef)trust_options
            )) {
            fozmo_catalog_error(
                error_buffer,
                error_capacity,
                @"The signed Fozmo Server executable needs Accessibility permission to select an exact catalog track in Music.app. Terminal permission alone does not cover this child process. Enable Fozmo Server (or fozmo) in System Settings → Privacy & Security → Accessibility, then retry."
            );
            return 0;
        }
        if (!CGPreflightPostEventAccess() && !CGRequestPostEventAccess()) {
            fozmo_catalog_error(
                error_buffer,
                error_capacity,
                @"Fozmo Server can inspect Music.app but macOS has not allowed it to post the exact-track click. Re-enable Fozmo Server in Privacy & Security → Accessibility, restart Fozmo, and retry."
            );
            return 0;
        }
        if (storefront == NULL || album_id == NULL || song_id == NULL) {
            fozmo_catalog_error(
                error_buffer,
                error_capacity,
                @"The Apple Music catalog identity is incomplete."
            );
            return 0;
        }

        NSString *storefront_string = [NSString stringWithUTF8String:storefront];
        NSString *album_string = [NSString stringWithUTF8String:album_id];
        NSString *song_string = [NSString stringWithUTF8String:song_id];
        NSString *url_string = [NSString stringWithFormat:
            @"music://music.apple.com/%@/album/x/%@?i=%@",
            storefront_string,
            album_string,
            song_string
        ];
        NSURL *url = [NSURL URLWithString:url_string];
        if (url == nil) {
            fozmo_catalog_error(
                error_buffer,
                error_capacity,
                @"Could not construct the Music.app catalog link."
            );
            return 0;
        }

        NSRunningApplication *previous_frontmost =
            NSWorkspace.sharedWorkspace.frontmostApplication;
        if (![NSWorkspace.sharedWorkspace openURL:url]) {
            fozmo_catalog_error(
                error_buffer,
                error_capacity,
                @"Music.app did not accept the catalog link."
            );
            return 0;
        }

        // Music ignores a catalog-row double-click delivered to an inactive
        // window. `NSRunningApplication.activateWithOptions:` reports success
        // from a CLI process without reliably making Music active, whereas
        // Music's AppleEvent `activate` command does. Restore the caller after
        // the click.
        NSDictionary *activation_error = nil;
        NSAppleScript *activation_script = [[NSAppleScript alloc]
            initWithSource:@"tell application \"Music\" to activate"
        ];
        if ([activation_script executeAndReturnError:&activation_error] == nil) {
            fozmo_catalog_error(
                error_buffer,
                error_capacity,
                @"Music.app rejected Fozmo's foreground activation command."
            );
            return 0;
        }

        const uint64_t deadline_ns =
            clock_gettime_nsec_np(CLOCK_UPTIME_RAW) + 12000000000ULL;
        NSRunningApplication *music_application = nil;
        while (clock_gettime_nsec_np(CLOCK_UPTIME_RAW) < deadline_ns) {
            music_application = [NSRunningApplication
                runningApplicationsWithBundleIdentifier:@"com.apple.Music"
            ].firstObject;
            if (music_application != nil) {
                break;
            }
            usleep(50000);
        }
        if (music_application == nil) {
            fozmo_restore_frontmost(previous_frontmost);
            fozmo_catalog_error(
                error_buffer,
                error_capacity,
                @"Music.app did not launch in time for exact track selection."
            );
            return 0;
        }
        while (![music_application isActive]
            && clock_gettime_nsec_np(CLOCK_UPTIME_RAW) < deadline_ns) {
            usleep(10000);
        }
        if (![music_application isActive]) {
            fozmo_restore_frontmost(previous_frontmost);
            fozmo_catalog_error(
                error_buffer,
                error_capacity,
                @"Music.app did not become active in time for exact track selection."
            );
            return 0;
        }

        // Music's parentId suffix is not stable across releases. macOS 26.5
        // currently exposes `track-list-<album>-undefined`, while older builds
        // used `track-list-<album>`. The album/song portion is stable and
        // uniquely identifies the requested row, so match that prefix.
        NSString *target_identifier_prefix = [NSString stringWithFormat:
            @"Music.shelfItem.AlbumTrackLockup[id=track-lockup-%@-%@,",
            album_string,
            song_string
        ];
        AXUIElementRef target = NULL;
        pid_t music_pid = 0;
        while (clock_gettime_nsec_np(CLOCK_UPTIME_RAW) < deadline_ns) {
            music_pid = (pid_t)fozmo_music_app_pid();
            if (music_pid > 0) {
                AXUIElementRef application = AXUIElementCreateApplication(music_pid);
                target = fozmo_find_accessibility_identifier_prefix(
                    application,
                    (__bridge CFStringRef)target_identifier_prefix
                );
                CFRelease(application);
                if (target != NULL) {
                    break;
                }
            }
            usleep(50000);
        }
        if (target == NULL || music_pid <= 0) {
            fozmo_restore_frontmost(previous_frontmost);
            fozmo_catalog_error(
                error_buffer,
                error_capacity,
                [NSString stringWithFormat:
                    @"Music.app did not expose catalog song %@ on album %@ within 12 seconds.",
                    song_string,
                    album_string
                ]
            );
            return 0;
        }

        CGPoint position = CGPointZero;
        CGSize size = CGSizeZero;
        const bool has_bounds =
            fozmo_accessibility_bounds(target, &position, &size);
        CFRelease(target);
        if (!has_bounds || size.width <= 0.0 || size.height <= 0.0) {
            fozmo_restore_frontmost(previous_frontmost);
            fozmo_catalog_error(
                error_buffer,
                error_capacity,
                @"Music.app exposed the requested catalog row without usable screen bounds."
            );
            return 0;
        }
        const CGPoint click_point = CGPointMake(
            position.x + size.width / 2.0,
            position.y + size.height / 2.0
        );
        if (!fozmo_post_double_click(click_point)) {
            fozmo_restore_frontmost(previous_frontmost);
            fozmo_catalog_error(
                error_buffer,
                error_capacity,
                @"Could not deliver the catalog-track activation to Music.app."
            );
            return 0;
        }
        // Quartz posting is asynchronous; let Music consume the completed
        // double-click before returning focus to the previous application.
        usleep(150000);
        fozmo_restore_frontmost(previous_frontmost);
        return 1;
    }
}

void *fozmo_process_tap_create(
    int32_t pid,
    uint32_t mute_original,
    double requested_sample_rate_hz,
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
        const double rate_delta =
            handle->format.mSampleRate > requested_sample_rate_hz
                ? handle->format.mSampleRate - requested_sample_rate_hz
                : requested_sample_rate_hz - handle->format.mSampleRate;
        if (requested_sample_rate_hz > 0 && rate_delta >= 0.5) {
            if (format_settable_status != kAudioHardwareNoError || !format_settable) {
                set_failure(
                    format_settable_status == kAudioHardwareNoError
                        ? FOZMO_STATUS_FORMAT_NOT_SETTABLE
                        : format_settable_status,
                    FOZMO_TAP_STAGE_TAP_FORMAT,
                    out_status,
                    out_stage
                );
                destroy_handle(handle);
                return NULL;
            }
            AudioStreamBasicDescription requested_format = handle->format;
            requested_format.mSampleRate = requested_sample_rate_hz;
            status = AudioObjectSetPropertyData(
                handle->tap_object_id,
                &property_address,
                0,
                NULL,
                sizeof(requested_format),
                &requested_format
            );
            if (status != kAudioHardwareNoError) {
                set_failure(
                    status,
                    FOZMO_TAP_STAGE_TAP_FORMAT,
                    out_status,
                    out_stage
                );
                destroy_handle(handle);
                return NULL;
            }

            property_size = sizeof(handle->format);
            status = AudioObjectGetPropertyData(
                handle->tap_object_id,
                &property_address,
                0,
                NULL,
                &property_size,
                &handle->format
            );
            const double applied_rate_delta =
                handle->format.mSampleRate > requested_sample_rate_hz
                    ? handle->format.mSampleRate - requested_sample_rate_hz
                    : requested_sample_rate_hz - handle->format.mSampleRate;
            if (status != kAudioHardwareNoError || applied_rate_delta >= 0.5) {
                set_failure(
                    status == kAudioHardwareNoError
                        ? FOZMO_STATUS_FORMAT_RATE_MISMATCH
                        : status,
                    FOZMO_TAP_STAGE_TAP_FORMAT,
                    out_status,
                    out_stage
                );
                destroy_handle(handle);
                return NULL;
            }
        }

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
