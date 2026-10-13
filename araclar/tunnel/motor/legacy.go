package main

import (
	"errors"
	"fmt"
	"os"
)

// Before v0.2.0 rabbithole was called tunel, and so were its service, paths
// and adapter (see legacy_*.go). Setting up takes over what tunel left: the
// server keeps its keys and users, so links already handed out keep working,
// and a client keeps its link, mode and autostart.

// legacySetup is what a tunel client left behind.
type legacySetup struct {
	found     bool
	running   bool // the tunnel was on, so it comes back on
	autostart bool
}

// cmdAdopt takes over a tunel setup on this computer.
func cmdAdopt() error {
	fmt.Println("  taking over the tunel setup; keys, users and links stay")
	if legacyServerInstalled() {
		if err := asAdmin("server"); err != nil {
			return err
		}
	}
	if legacyClientInstalled() {
		if err := asAdmin("adopt"); err != nil {
			return err
		}
		fmt.Println()
		return cmdStatus()
	}
	return nil
}

func adminAdopt() error {
	lg, err := installClientAdopting()
	if err != nil {
		return err
	}
	ok("installed %s", installedBin)
	if lg.running {
		return svcStart(currentMode())
	}
	return nil
}

// installClientAdopting installs the client, taking over a tunel client first.
func installClientAdopting() (legacySetup, error) {
	lg, err := takeLegacyClient()
	if err != nil {
		return lg, err
	}
	if lg.found {
		ok("took over the tunel client and removed it")
	}
	if err := installClient(); err != nil {
		return lg, err
	}
	if lg.autostart {
		return lg, setAutostart(true)
	}
	return lg, nil
}

// moveIfAbsent moves old to cur, unless cur is already there; then old just goes.
func moveIfAbsent(old, cur string) error {
	raw, err := os.ReadFile(old)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	if _, err := os.Stat(cur); errors.Is(err, os.ErrNotExist) {
		if err := writeFileAtomic(cur, raw, 0o600); err != nil {
			return err
		}
	}
	return os.Remove(old)
}
