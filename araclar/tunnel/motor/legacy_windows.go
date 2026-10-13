package main

import (
	"os"
	"path/filepath"
	"time"

	"golang.org/x/sys/windows"
	"golang.org/x/sys/windows/svc"
	"golang.org/x/sys/windows/svc/mgr"
)

const legacySvcName = "tunel"

var (
	legacyInstallDir = filepath.Join(os.Getenv("ProgramFiles"), "tunel")
	legacyDataDir    = filepath.Join(os.Getenv("ProgramData"), "tunel")
)

// A tunel server only ever ran on Linux.
func legacyServerInstalled() bool     { return false }
func legacyServerState() *ServerState { return nil }
func removeLegacyServer()             {}

func legacyClientInstalled() bool {
	_, done, err := openNamedService(legacySvcName, windows.SERVICE_QUERY_STATUS)
	if err != nil {
		return false
	}
	done()
	return true
}

// takeLegacyClient stops and deletes the tunel service, moves its link, mode
// and marks to where rabbithole keeps them, and removes the rest.
func takeLegacyClient() (legacySetup, error) {
	var lg legacySetup
	if !legacyClientInstalled() {
		return lg, nil
	}
	lg.found = true
	m, err := mgr.Connect()
	if err != nil {
		return lg, err
	}
	defer m.Disconnect()
	s, err := m.OpenService(legacySvcName)
	if err != nil {
		return lg, err
	}
	if st, err := s.Query(); err == nil {
		lg.running = st.State == svc.Running || st.State == svc.StartPending
	}
	if cfg, err := s.Config(); err == nil {
		lg.autostart = cfg.StartType == mgr.StartAutomatic
	}
	s.Control(svc.Stop)
	for i := 0; i < 150; i++ {
		if st, err := s.Query(); err != nil || st.State == svc.Stopped {
			break
		}
		time.Sleep(100 * time.Millisecond)
	}
	s.Delete()
	s.Close()

	for name, cur := range map[string]string{
		"client.json":               clientStatePath,
		"mode":                      clientModePath,
		"off":                       offFlagPath,
		"wintun-installed-by-tunel": wintunMarker, // the driver is ours to remove later
	} {
		if err := moveIfAbsent(filepath.Join(legacyDataDir, name), cur); err != nil {
			return lg, err
		}
	}
	removeAdapter(legacySvcName)
	if _, err := editSystemPath(legacyInstallDir, false); err != nil {
		bad("PATH: %v", err)
	}
	os.RemoveAll(legacyDataDir)
	if os.RemoveAll(legacyInstallDir) != nil {
		deleteLater(legacyInstallDir)
	}
	return lg, nil
}
