#import <Foundation/Foundation.h>
#import <dispatch/dispatch.h>
#import <libproc.h>
#import <stdint.h>
#import <stdio.h>
#import <stdlib.h>
#import <string.h>
#import <unistd.h>

/// The Music.app executable, distinct from the bundle's `.appex` plug-ins,
/// which live under `Contents/PlugIns` and must never be mistaken for it.
static const char kFozmoMusicExecutableSuffix[] = "/Music.app/Contents/MacOS/Music";

/// Ask the kernel directly rather than `NSRunningApplication`.
///
/// `NSWorkspace` keeps its running-application list current by processing
/// notifications on a run loop. Fozmo's main thread runs the async runtime and
/// never pumps one, so that list can go stale after the catalog activation path
/// touches `NSWorkspace` — reporting a running Music.app as terminated and
/// aborting playback with a spurious "Music.app quit". Playback start and the
/// transport monitor both poll this several times per second, so it also must
/// not fork a helper process.
int32_t fozmo_music_app_pid(void) {
    int capacity = proc_listpids(PROC_ALL_PIDS, 0, NULL, 0);
    if (capacity <= 0) {
        return 0;
    }
    // Processes can start between sizing the buffer and filling it.
    capacity += 64 * (int)sizeof(pid_t);
    pid_t *pids = calloc(1, (size_t)capacity);
    if (pids == NULL) {
        return 0;
    }
    const int bytes = proc_listpids(PROC_ALL_PIDS, 0, pids, capacity);
    int32_t found = 0;
    if (bytes > 0) {
        const int count = bytes / (int)sizeof(pid_t);
        const size_t suffix_length = strlen(kFozmoMusicExecutableSuffix);
        char path[PROC_PIDPATHINFO_MAXSIZE];
        for (int index = 0; index < count && found == 0; index += 1) {
            if (pids[index] <= 0) {
                continue;
            }
            if (proc_pidpath(pids[index], path, sizeof(path)) <= 0) {
                continue;
            }
            const size_t length = strlen(path);
            if (length >= suffix_length
                && strcmp(path + length - suffix_length, kFozmoMusicExecutableSuffix) == 0) {
                found = pids[index];
            }
        }
    }
    free(pids);
    return found;
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

/// Compile AppleScript once per source and execute it in-process. Every script
/// is wrapped in an Apple Event timeout so a wedged Music.app cannot pin a
/// Tokio blocking thread for the system's roughly two-minute default.
int32_t fozmo_music_execute_script(
    const char *source,
    double timeout_seconds,
    char *output_buffer,
    size_t output_capacity,
    char *error_buffer,
    size_t error_capacity
) {
    @autoreleasepool {
        if (output_buffer != NULL && output_capacity > 0) {
            output_buffer[0] = '\0';
        }
        if (error_buffer != NULL && error_capacity > 0) {
            error_buffer[0] = '\0';
        }
        if (source == NULL) {
            fozmo_catalog_error(
                error_buffer,
                error_capacity,
                @"The Music.app AppleScript source is missing."
            );
            return 0;
        }
        NSString *body = [NSString stringWithUTF8String:source];
        if (body == nil) {
            fozmo_catalog_error(
                error_buffer,
                error_capacity,
                @"The Music.app AppleScript source is not valid UTF-8."
            );
            return 0;
        }
        const double bounded_timeout =
            isfinite(timeout_seconds) && timeout_seconds > 0.0
                ? fmin(timeout_seconds, 30.0)
                : 2.0;
        NSString *wrapped = [NSString stringWithFormat:
            @"with timeout of %.3f seconds\n%@\nend timeout",
            bounded_timeout,
            body
        ];

        static NSMutableDictionary<NSString *, NSAppleScript *> *scripts;
        static NSLock *scripts_lock;
        static dispatch_once_t once;
        dispatch_once(&once, ^{
            scripts = [NSMutableDictionary dictionary];
            scripts_lock = [[NSLock alloc] init];
        });

        NSDictionary *compile_error = nil;
        [scripts_lock lock];
        NSAppleScript *script = scripts[wrapped];
        if (script == nil) {
            script = [[NSAppleScript alloc] initWithSource:wrapped];
            if (![script compileAndReturnError:&compile_error]) {
                script = nil;
            } else {
                scripts[wrapped] = script;
            }
        }
        [scripts_lock unlock];
        if (script == nil) {
            NSString *message =
                compile_error[NSAppleScriptErrorMessage]
                    ?: @"Music.app AppleScript compilation failed.";
            fozmo_catalog_error(error_buffer, error_capacity, message);
            return 0;
        }

        NSDictionary *execution_error = nil;
        NSAppleEventDescriptor *result = nil;
        @synchronized (script) {
            result = [script executeAndReturnError:&execution_error];
        }
        if (result == nil) {
            NSString *message =
                execution_error[NSAppleScriptErrorMessage]
                    ?: @"The Music app command failed.";
            NSNumber *number = execution_error[NSAppleScriptErrorNumber];
            if (number != nil) {
                message = [NSString stringWithFormat:
                    @"Music got an error: %@ (%@)",
                    message,
                    number
                ];
            }
            fozmo_catalog_error(error_buffer, error_capacity, message);
            return 0;
        }
        if (output_buffer != NULL && output_capacity > 0) {
            NSString *value = result.stringValue ?: @"";
            const char *utf8 = value.UTF8String;
            snprintf(
                output_buffer,
                output_capacity,
                "%s",
                utf8 != NULL ? utf8 : ""
            );
        }
        return 1;
    }
}

static NSCondition *gFozmoMusicNotificationCondition;
static BOOL gFozmoMusicNotificationPending;

static NSCondition *fozmo_music_notification_condition(void) {
    static id observer;
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        gFozmoMusicNotificationCondition = [[NSCondition alloc] init];
        gFozmoMusicNotificationPending = NO;
        observer = [NSDistributedNotificationCenter.defaultCenter
            addObserverForName:@"com.apple.Music.playerInfo"
            object:nil
            queue:nil
            usingBlock:^(NSNotification *notification) {
                (void)notification;
                [gFozmoMusicNotificationCondition lock];
                gFozmoMusicNotificationPending = YES;
                [gFozmoMusicNotificationCondition broadcast];
                [gFozmoMusicNotificationCondition unlock];
            }
        ];
        (void)observer;
    });
    return gFozmoMusicNotificationCondition;
}

/// Block a non-async worker until Music.app pushes a transport/track event.
/// A timeout is retained as a liveness fallback for app termination and missed
/// distributed notifications.
int32_t fozmo_music_wait_for_player_notification(uint32_t timeout_ms) {
    @autoreleasepool {
        NSCondition *condition = fozmo_music_notification_condition();
        NSDate *deadline = [NSDate
            dateWithTimeIntervalSinceNow:
                (double)(timeout_ms > 0 ? timeout_ms : 1) / 1000.0
        ];
        [condition lock];
        if (!gFozmoMusicNotificationPending) {
            [condition waitUntilDate:deadline];
        }
        const BOOL notified = gFozmoMusicNotificationPending;
        gFozmoMusicNotificationPending = NO;
        [condition unlock];
        return notified ? 1 : 0;
    }
}
