#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
#import <Foundation/Foundation.h>
#import <libproc.h>
#import <math.h>
#import <stdint.h>
#import <stdio.h>
#import <stdlib.h>
#import <string.h>
#import <time.h>
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

/// Seed the search from Music.app's windows when it exposes any. The
/// application element also parents the menu bar, and walking that deep menu
/// tree costs thousands of accessibility round trips on every retry before the
/// catalog track list is ever reached.
static NSArray *fozmo_accessibility_search_roots(AXUIElementRef application) {
    CFTypeRef windows_value = NULL;
    if (AXUIElementCopyAttributeValue(
            application,
            kAXWindowsAttribute,
            &windows_value
        ) == kAXErrorSuccess
        && windows_value != NULL) {
        NSArray *windows =
            CFGetTypeID(windows_value) == CFArrayGetTypeID()
                ? [(__bridge NSArray *)windows_value copy]
                : nil;
        CFRelease(windows_value);
        if (windows.count > 0) {
            return windows;
        }
    }
    return @[(__bridge id)application];
}

static AXUIElementRef fozmo_find_accessibility_identifier_prefix(
    AXUIElementRef application,
    CFStringRef target_identifier_prefix
) {
    NSMutableArray *queue = [NSMutableArray
        arrayWithArray:fozmo_accessibility_search_roots(application)
    ];
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

/// How long to let the row's bounds stop moving. Two calm samples cost about
/// 120ms; the rest of the budget only gets spent while the page animates.
static const uint64_t kFozmoBoundsSettleBudgetNs = 1500000000ULL;

/// Sub-pixel drift still clicks the right row, and an animating page may never
/// report two byte-identical frames.
static const double kFozmoBoundsSettleTolerance = 1.0;

/// Track the element's bounds until they stop moving, reporting the most recent
/// usable reading either way.
///
/// Music.app is still laying out and scrolling the album page when the row
/// first appears in the accessibility tree, so bounds sampled at discovery time
/// describe where the row *was*. Clicking those stale coordinates lands on a
/// neighbouring row, or on nothing, and Music.app stays on its current track.
///
/// Settling is best effort. A page that never stops moving still yields a
/// better click target than giving up does, and the caller checks the point
/// against Music.app's window before posting anything. Returns whether any
/// usable bounds were seen at all.
static bool fozmo_accessibility_settled_bounds(
    AXUIElementRef element,
    CGPoint *position,
    CGSize *size
) {
    const uint64_t deadline_ns =
        clock_gettime_nsec_np(CLOCK_UPTIME_RAW) + kFozmoBoundsSettleBudgetNs;
    CGPoint previous_position = CGPointZero;
    CGSize previous_size = CGSizeZero;
    bool has_previous = false;
    // Sample at least once: the caller's search may already have spent the
    // whole activation budget, and stale bounds still beat no bounds.
    do {
        CGPoint current_position = CGPointZero;
        CGSize current_size = CGSizeZero;
        if (fozmo_accessibility_bounds(element, &current_position, &current_size)
            && current_size.width > 0.0
            && current_size.height > 0.0) {
            *position = current_position;
            *size = current_size;
            if (has_previous
                && fabs(current_position.x - previous_position.x)
                    <= kFozmoBoundsSettleTolerance
                && fabs(current_position.y - previous_position.y)
                    <= kFozmoBoundsSettleTolerance
                && fabs(current_size.width - previous_size.width)
                    <= kFozmoBoundsSettleTolerance
                && fabs(current_size.height - previous_size.height)
                    <= kFozmoBoundsSettleTolerance) {
                return true;
            }
            previous_position = current_position;
            previous_size = current_size;
            has_previous = true;
        }
        usleep(60000);
    } while (clock_gettime_nsec_np(CLOCK_UPTIME_RAW) < deadline_ns);
    return has_previous;
}

/// Whether the click would land inside Music.app's own window.
///
/// A row scrolled out of the viewport keeps valid accessibility bounds that sit
/// outside the window, so posting a click there would miss Music.app entirely
/// and deliver a double-click to whatever app happens to own that point.
/// Unknown geometry must not block selection, so only a confident miss fails.
static bool fozmo_point_is_inside_focused_window(
    AXUIElementRef application,
    CGPoint point
) {
    CFTypeRef window = NULL;
    if (AXUIElementCopyAttributeValue(
            application,
            kAXFocusedWindowAttribute,
            &window
        ) != kAXErrorSuccess
        || window == NULL) {
        return true;
    }
    CGPoint origin = CGPointZero;
    CGSize size = CGSizeZero;
    const bool known =
        fozmo_accessibility_bounds((AXUIElementRef)window, &origin, &size);
    CFRelease(window);
    if (!known || size.width <= 0.0 || size.height <= 0.0) {
        return true;
    }
    return point.x >= origin.x
        && point.x <= origin.x + size.width
        && point.y >= origin.y
        && point.y <= origin.y + size.height;
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
        // Separate the two clicks so Music.app recognises a double click. The
        // pair is already complete after the second one, so do not pay the
        // delay again on the way out.
        if (click_state == 1) {
            usleep(90000);
        }
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
        AXUIElementRef music_ax_application = NULL;
        pid_t music_pid = 0;
        while (clock_gettime_nsec_np(CLOCK_UPTIME_RAW) < deadline_ns) {
            music_pid = (pid_t)fozmo_music_app_pid();
            if (music_pid > 0) {
                AXUIElementRef application = AXUIElementCreateApplication(music_pid);
                // Music.app is still fetching and rendering the album page, so
                // individual accessibility messages can be slow. Cap them well
                // under the overall deadline: one stalled reply must not spend
                // the whole activation budget.
                AXUIElementSetMessagingTimeout(application, 1.0);
                target = fozmo_find_accessibility_identifier_prefix(
                    application,
                    (__bridge CFStringRef)target_identifier_prefix
                );
                if (target != NULL) {
                    // Kept alive past the search to locate the window the click
                    // has to land in.
                    music_ax_application = application;
                    break;
                }
                CFRelease(application);
            }
            usleep(50000);
        }
        if (target == NULL || music_ax_application == NULL || music_pid <= 0) {
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

        // A row below the fold keeps valid bounds outside the viewport, so ask
        // Music.app to bring it on screen before trusting any coordinates.
        // Spelled literally because AppKit only declares a named constant for
        // this action from macOS 26, well above this bridge's deployment
        // target. Elements that do not support it just report an error.
        AXUIElementPerformAction(target, CFSTR("AXScrollToVisible"));

        CGPoint position = CGPointZero;
        CGSize size = CGSizeZero;
        const bool has_bounds =
            fozmo_accessibility_settled_bounds(target, &position, &size);
        CFRelease(target);
        if (!has_bounds) {
            CFRelease(music_ax_application);
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
        const bool click_lands_in_music =
            fozmo_point_is_inside_focused_window(music_ax_application, click_point);
        CFRelease(music_ax_application);
        if (!click_lands_in_music) {
            fozmo_restore_frontmost(previous_frontmost);
            fozmo_catalog_error(
                error_buffer,
                error_capacity,
                @"The requested catalog row stayed outside Music.app's window, so Fozmo did not click blind."
            );
            return 0;
        }
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
