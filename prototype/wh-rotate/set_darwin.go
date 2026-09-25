//go:build darwin && cgo

package main

/*
#cgo CFLAGS: -x objective-c -fobjc-arc
#cgo LDFLAGS: -framework AppKit -framework Foundation
#import <AppKit/AppKit.h>

// Sets the desktop image on every attached screen with NSWorkspace, the same
// API the system Settings pane uses. The "allSpaces" option key is the private
// but long-stable way to write every Space, so new Spaces inherit it too.
static int setWallpaperAllScreens(const char *cpath, int allSpaces) {
    @autoreleasepool {
        NSURL *url = [NSURL fileURLWithPath:[NSString stringWithUTF8String:cpath]];
        NSDictionary *opts = allSpaces ? @{ @"allSpaces" : @YES } : @{};
        int fails = 0;
        for (NSScreen *screen in [NSScreen screens]) {
            NSError *err = nil;
            if (![[NSWorkspace sharedWorkspace] setDesktopImageURL:url
                                                        forScreen:screen
                                                          options:opts
                                                            error:&err]) {
                fails++;
            }
        }
        return fails;
    }
}
*/
import "C"

import (
	"fmt"
	"unsafe"
)

func platformSet(path string, allSpaces bool) error {
	cpath := C.CString(path)
	defer C.free(unsafe.Pointer(cpath))
	as := C.int(0)
	if allSpaces {
		as = 1
	}
	if fails := int(C.setWallpaperAllScreens(cpath, as)); fails > 0 {
		return fmt.Errorf("NSWorkspace failed on %d screen(s)", fails)
	}
	return nil
}
