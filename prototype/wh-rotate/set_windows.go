//go:build windows

package main

import (
	"fmt"
	"os/exec"
	"syscall"
	"unsafe"
)

var (
	user32           = syscall.NewLazyDLL("user32.dll")
	systemParameters = user32.NewProc("SystemParametersInfoW")
)

const (
	spiSetDeskWallpaper = 0x0014
	spifUpdateIniFile   = 0x01
	spifSendChange      = 0x02
)

func platformSet(path string, allSpaces bool) error {
	// Windows has no per-virtual-desktop wallpaper: one image covers all of them.
	_ = allSpaces

	// Scale mode lives in the registry; 10 = Fill. Done first so the image
	// appears already fitted.
	for _, kv := range [][2]string{
		{"WallpaperStyle", "10"},
		{"TileWallpaper", "0"},
	} {
		_ = exec.Command("reg", "add", `HKCU\Control Panel\Desktop`,
			"/v", kv[0], "/t", "REG_SZ", "/d", kv[1], "/f").Run()
	}

	p, err := syscall.UTF16PtrFromString(path)
	if err != nil {
		return err
	}
	ret, _, callErr := systemParameters.Call(
		spiSetDeskWallpaper,
		0,
		uintptr(unsafe.Pointer(p)),
		spifUpdateIniFile|spifSendChange,
	)
	if ret == 0 {
		return fmt.Errorf("SystemParametersInfoW failed: %v", callErr)
	}
	return nil
}
