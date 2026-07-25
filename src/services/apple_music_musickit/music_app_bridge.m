#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
#import <Foundation/Foundation.h>
#import <stdint.h>
#import <stdio.h>
#import <time.h>
#import <unistd.h>

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
        usleep(150000);
        fozmo_restore_frontmost(previous_frontmost);
        return 1;
    }
}
