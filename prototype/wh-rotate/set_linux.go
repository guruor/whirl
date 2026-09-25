//go:build linux

package main

import (
	"fmt"
	"os"
	"os/exec"
	"strings"
)

func have(bin string) bool {
	_, err := exec.LookPath(bin)
	return err == nil
}

// Linux has no single wallpaper API: it depends on the desktop environment.
// GNOME/KDE/xfce are handled without a resident process. Wayland compositors
// need a daemon (swaybg/hyprpaper), which is Linux's own design, not ours.
func platformSet(path string, allSpaces bool) error {
	_ = allSpaces
	switch {
	case have("gsettings") && os.Getenv("XDG_CURRENT_DESKTOP") != "KDE":
		return run("gsettings", "set", "org.gnome.desktop.background", "picture-uri",
			"file://"+path)
	case have("qdbus"):
		script := fmt.Sprintf(
			`var d=desktops();for(i=0;i<d.length;i++){d[i].wallpaperPlugin="org.kde.image";d[i].currentConfigGroup=["Wallpaper","org.kde.image","General"];d[i].writeConfig("Image","file://%s")}`,
			path)
		return run("qdbus", "org.kde.plasmashell", "/PlasmaShell",
			"org.kde.PlasmaShell.evaluateScript", script)
	case have("swaybg"):
		_ = run("pkill", "-x", "swaybg")
		cmd := exec.Command("swaybg", "-i", path, "-m", "fill")
		return cmd.Start() // stays resident: Wayland needs a surface owner
	case have("hyprpaper"):
		_ = run("hyprctl", "hyprpaper", "unload", "all")
		_ = run("hyprctl", "hyprpaper", "preload", path)
		return run("hyprctl", "hyprpaper", "wallpaper", ", "+path)
	case have("feh"):
		return run("feh", "--bg-fill", path)
	}
	return fmt.Errorf("no supported wallpaper setter found (tried gsettings, qdbus, swaybg, hyprpaper, feh)")
}

func run(name string, args ...string) error {
	cmd := exec.Command(name, args...)
	var errb strings.Builder
	cmd.Stderr = &errb
	if err := cmd.Run(); err != nil {
		return fmt.Errorf("%s: %v %s", name, err, strings.TrimSpace(errb.String()))
	}
	return nil
}
