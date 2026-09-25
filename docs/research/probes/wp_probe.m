// wp_probe.m -- read-only probe: what does NSWorkspace think the desktop image is?
//
// Build: clang -fobjc-arc -framework AppKit -framework Foundation -framework CoreGraphics \
//              -o wp_probe wp_probe.m
// Run:   ./wp_probe
//
// Prints, per attached screen: the CGDisplay UUID (the key Index.plist uses for its
// Displays[<uuid>] nodes), the frame, the image NSWorkspace reports, the option dictionary
// it reports, and whether the file behind the URL still exists. The "file exists" line
// matters: macOS keeps showing a cached render after the image is deleted, so a rotator
// that trusts the URL alone can point at nothing.

#import <AppKit/AppKit.h>
#import <Foundation/Foundation.h>

int main(void) {
    @autoreleasepool {
        NSArray<NSScreen *> *screens = NSScreen.screens;
        printf("NSScreen.screens count: %lu\n", (unsigned long)screens.count);

        for (NSScreen *screen in screens) {
            printf("\nscreen: %s\n", screen.localizedName.UTF8String);

            NSNumber *screenNumber = screen.deviceDescription[@"NSScreenNumber"];
            printf("  NSScreenNumber : %s\n", screenNumber.stringValue.UTF8String);

            CGDirectDisplayID displayID = (CGDirectDisplayID)screenNumber.unsignedIntValue;
            CFUUIDRef uuid = CGDisplayCreateUUIDFromDisplayID(displayID);
            if (uuid) {
                CFStringRef text = CFUUIDCreateString(kCFAllocatorDefault, uuid);
                printf("  CGDisplay UUID : %s\n", [(__bridge NSString *)text UTF8String]);
                CFRelease(text);
                CFRelease(uuid);
            }

            NSRect frame = screen.frame;
            printf("  frame          : %.0fx%.0f at (%.0f,%.0f)  backingScaleFactor=%g\n",
                   frame.size.width, frame.size.height, frame.origin.x, frame.origin.y,
                   screen.backingScaleFactor);

            NSWorkspace *workspace = NSWorkspace.sharedWorkspace;
            NSURL *url = [workspace desktopImageURLForScreen:screen];
            NSDictionary *options = [workspace desktopImageOptionsForScreen:screen];
            printf("  desktopImageURL: %s\n", url.path.UTF8String ?: "(nil)");
            printf("  options        : %s\n", options.description.UTF8String ?: "(nil)");
            printf("  file exists    : %s\n",
                   url && [NSFileManager.defaultManager fileExistsAtPath:url.path] ? "yes" : "no");
        }
    }
    return 0;
}
