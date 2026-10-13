package main

import (
	"encoding/json"
	"os"
	"os/exec"
)

const (
	legacyBin            = "/usr/local/bin/tunel"
	legacyDir            = "/etc/tunel"
	legacyServerUnit     = "tunel-server"
	legacyServerUnitPath = "/etc/systemd/system/tunel-server.service"
	legacyClientUnit     = "tunel"
	legacyClientUnitPath = "/etc/systemd/system/tunel.service"
)

func legacyServerInstalled() bool {
	_, err := os.Stat(legacyServerUnitPath)
	return err == nil
}

func legacyClientInstalled() bool {
	_, err := os.Stat(legacyClientUnitPath)
	return err == nil
}

// legacyServerState returns the keys and users of a tunel server, or nil, and
// stops that server to free its port. removeLegacyServer deletes it once the
// state is saved again.
func legacyServerState() *ServerState {
	raw, err := os.ReadFile(legacyDir + "/server.json")
	if err != nil {
		return nil
	}
	s := &ServerState{}
	if json.Unmarshal(raw, s) != nil || len(s.Users) == 0 {
		return nil
	}
	exec.Command("systemctl", "disable", "--now", legacyServerUnit).Run()
	return s
}

func removeLegacyServer() {
	if !legacyServerInstalled() {
		return
	}
	exec.Command("systemctl", "disable", "--now", legacyServerUnit).Run()
	os.Remove(legacyServerUnitPath)
	os.Remove(legacyDir + "/server.json")
	systemctl("daemon-reload")
	removeLegacyLeftovers()
}

// takeLegacyClient stops and removes a tunel client and moves its link, mode
// and off mark to where rabbithole keeps them.
func takeLegacyClient() (legacySetup, error) {
	var lg legacySetup
	if !legacyClientInstalled() {
		return lg, nil
	}
	lg.found = true
	lg.running = exec.Command("systemctl", "is-active", "--quiet", legacyClientUnit).Run() == nil
	lg.autostart = exec.Command("systemctl", "is-enabled", "--quiet", legacyClientUnit).Run() == nil
	exec.Command("systemctl", "disable", "--now", legacyClientUnit).Run()
	for old, cur := range map[string]string{
		legacyDir + "/client.json": clientStatePath,
		legacyDir + "/mode":        clientModePath,
		legacyDir + "/off":         offFlagPath,
	} {
		if err := moveIfAbsent(old, cur); err != nil {
			return lg, err
		}
	}
	os.Remove(legacyClientUnitPath)
	os.Remove("/run/tunel-explicit-start")
	systemctl("daemon-reload")
	removeLegacyLeftovers()
	return lg, nil
}

// removeLegacyLeftovers deletes the tunel binary and folder once neither the
// server nor the client needs them.
func removeLegacyLeftovers() {
	if legacyServerInstalled() || legacyClientInstalled() {
		return
	}
	os.Remove(legacyDir) // only if empty
	os.Remove(legacyBin)
}
