// wp_set.m -- write probe: set the desktop image on every attached screen, then read back.
//
// Build: clang -fobjc-arc -framework AppKit -framework Foundation -framework CoreGraphics \
//              -o wp_set wp_set.m
// Run:   ./wp_set <image-path> [allspaces]
//
// `allspaces` passes the undocumented option key @"allSpaces". The research note reports
// that it changes no observable state on macOS 26.5.2, that the same two Index.plist nodes
// are written with or without it, so it exists here only to reproduce that measurement.
// Do not ship it.
//
// Read the error, not just the BOOL: a path that does not exist returns NO with
// "The file doesn't exist." and writes nothing to the store.

#import <AppKit/AppKit.h>
#import <Foundation/Foundation.h>

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        if (argc < 2) {
            fprintf(stderr, "usage: %s <image-path> [allspaces]\n", argv[0]);
            return 2;
        }
        NSString *path = @(argv[1]);
        BOOL allSpaces = argc > 2 && strcmp(argv[2], "allspaces") == 0;

        NSURL *url = [NSURL fileURLWithPath:path];
        NSDictionary *options = allSpaces ? @{@"allSpaces" : @YES} : @{};

        printf("allSpaces option passed: %s\n", allSpaces ? "yes" : "no");
        printf("image: %s\n", path.UTF8String);

        for (NSScreen *screen in NSScreen.screens) {
            NSError *error = nil;
            BOOL ok = [NSWorkspace.sharedWorkspace setDesktopImageURL:url
                                                            forScreen:screen
                                                              options:options
                                                                error:&error];
            printf("  screen %s -> %s%s%s\n", screen.localizedName.UTF8String,
                   ok ? "OK" : "FAILED", error ? " error=" : "",
                   error ? error.localizedDescription.UTF8String : "");
        }

        // WallpaperAgent applies the change asynchronously; give it a beat before reading.
        [NSThread sleepForTimeInterval:1.5];
        for (NSScreen *screen in NSScreen.screens) {
            NSURL *back = [NSWorkspace.sharedWorkspace desktopImageURLForScreen:screen];
            NSDictionary *backOptions =
                [NSWorkspace.sharedWorkspace desktopImageOptionsForScreen:screen];
            printf("  readback %s -> %s\n", screen.localizedName.UTF8String,
                   back.path.UTF8String ?: "(nil)");
            printf("           options -> %s\n", backOptions.description.UTF8String ?: "(nil)");
        }
    }
    return 0;
}
