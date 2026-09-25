//go:build (darwin && !cgo) || (!darwin && !windows && !linux)

package main

import (
	"fmt"
	"runtime"
)

func platformSet(path string, allSpaces bool) error {
	if runtime.GOOS == "darwin" {
		return fmt.Errorf("this binary was built without cgo; macOS needs CGO_ENABLED=1 go build")
	}
	return fmt.Errorf("unsupported platform: %s", runtime.GOOS)
}
