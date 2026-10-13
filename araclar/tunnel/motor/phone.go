package main

import (
	"bytes"
	"compress/gzip"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/url"
	"os"
	"path/filepath"
	"strconv"
	"strings"

	"rsc.io/qr"
)

// Phones run the same two modes in the official sing-box app, with the same
// configuration as a computer; only the tunnel adapter differs. The dpi
// profile holds no secret, so every release publishes it and the app keeps
// it up to date from there. A server profile holds the user's key, so
// `rabbithole phone` writes it to a file for the user to carry over.

// dpiProfileAsset is the release file the dpi profile is published as.
const dpiProfileAsset = "rabbithole-dpi.json"

var dpiProfileURL = releases + "/latest/download/" + dpiProfileAsset

// phoneTun is tunInbound for the sing-box app: the app names the adapter,
// picks the stack and keeps its own routes out of the tunnel.
func phoneTun() obj {
	return obj{
		"type": "tun", "tag": "tun",
		"address":               []string{"198.18.0.1/30", "fdfe:dcba:9876::1/126"},
		"auto_route":            true,
		"route_exclude_address": lanRanges,
	}
}

// forPhone turns a computer's tunnel configuration into a phone's.
func forPhone(cfg obj) obj {
	cfg["inbounds"] = []obj{phoneTun()}
	return cfg
}

// phoneDPIConfig cannot pick a DoH server on the network it runs on, as a
// computer does at each start, so it takes the most preferred one.
func phoneDPIConfig() obj {
	return forPhone(dpiConfig(0, "", dohServers[0]))
}

// printDPIProfile prints the dpi profile as build.sh publishes it.
func printDPIProfile() error {
	raw, err := json.MarshalIndent(phoneDPIConfig(), "", "  ")
	if err != nil {
		return err
	}
	_, err = fmt.Printf("%s\n", raw)
	return err
}

func phoneServerConfig(l Link) obj {
	return forPhone(clientConfig(l, 0, ""))
}

// profileFile encodes a local profile the way the sing-box app shares one
// (a .bpf file; see ProfileContent in sing-box's experimental/libbox), so
// opening the file on a phone imports it.
func profileFile(name string, cfg obj) ([]byte, error) {
	raw, err := json.MarshalIndent(cfg, "", "  ")
	if err != nil {
		return nil, err
	}
	var b bytes.Buffer
	b.WriteByte(3) // message type: profile content
	b.WriteByte(1) // version
	gz := gzip.NewWriter(&b)
	writeString := func(s string) {
		var n [binary.MaxVarintLen64]byte
		gz.Write(n[:binary.PutUvarint(n[:], uint64(len(s)))])
		io.WriteString(gz, s)
	}
	writeString(name)
	binary.Write(gz, binary.BigEndian, int32(0)) // profile type: local
	writeString(string(raw))
	if err := gz.Close(); err != nil {
		return nil, err
	}
	return b.Bytes(), nil
}

// dpiImportLink opens the sing-box app and adds the published dpi profile.
func dpiImportLink() string {
	u := url.URL{
		Scheme:   "sing-box",
		Host:     "import-remote-profile",
		RawQuery: url.Values{"url": {dpiProfileURL}}.Encode(),
		Fragment: "rabbithole dpi",
	}
	return u.String()
}

// ---------- rabbithole phone [LINK] ----------

func cmdPhone(args []string) error {
	var l *Link
	switch len(args) {
	case 0:
		if hasLink() {
			saved, err := loadClient()
			if errors.Is(err, os.ErrPermission) {
				return fmt.Errorf("only root can read this computer's link; run: sudo rabbithole phone")
			}
			if err != nil {
				return err
			}
			l = &saved
		}
	case 1:
		parsed, err := ParseLink(args[0])
		if err != nil {
			return err
		}
		l = &parsed
	default:
		return fmt.Errorf("usage: rabbithole phone ['vless://…'] (quote the link)")
	}

	fmt.Printf("\n  On the phone, install the %ssing-box%s app (App Store, Google Play; see sing-box.sagernet.org/clients).\n", cBold, cReset)
	fmt.Printf("  It runs the same two modes as a computer, each as a profile; the one you start is the mode.\n\n")

	fmt.Printf("  %sdpi%s  no server. Scan this with the phone's camera:\n\n", cBold, cReset)
	if !printQR(dpiImportLink()) {
		fmt.Printf("       %s\n", dpiImportLink())
	}
	fmt.Printf("\n       or in sing-box add a remote profile with this URL:\n")
	fmt.Printf("       %s\n", dpiProfileURL)
	fmt.Printf("       %sthe app keeps it up to date from there%s\n\n", cDim, cReset)

	if l == nil {
		fmt.Printf("  %son%s   through your server: rabbithole phone 'vless://…' makes its profile.\n", cBold, cReset)
		fmt.Printf("       %sHiddify and v2rayNG take the link itself, too.%s\n\n", cDim, cReset)
		return nil
	}
	data, err := profileFile("rabbithole on", phoneServerConfig(*l))
	if err != nil {
		return err
	}
	path, err := saveToDesktop("rabbithole-"+l.Name+".bpf", data)
	if err != nil {
		return err
	}
	fmt.Printf("  %son%s   through %s. In sing-box, open this file on the phone (send it there):\n\n", cBold, cReset, l.Host)
	fmt.Printf("       %s\n\n", path)
	fmt.Printf("       or in Hiddify or v2rayNG, scan this with the app's own QR scanner:\n\n")
	if !printQR(l.String()) {
		fmt.Printf("       %s\n", l)
	}
	fmt.Printf("\n       %sBoth hold the key to the server: show the code only to your own phone,\n", cYellow)
	fmt.Printf("       send the file only to yourself, then delete it.%s\n\n", cReset)
	return nil
}

// saveToDesktop writes a private file where it is easy to find, owned by
// the user even under sudo.
func saveToDesktop(name string, data []byte) (string, error) {
	home, err := userHome()
	if err != nil {
		return "", err
	}
	path := filepath.Join(desktopDir(home), name)
	if err := os.WriteFile(path, data, 0o600); err != nil {
		return "", err
	}
	if uid, err := strconv.Atoi(os.Getenv("SUDO_UID")); err == nil && os.Geteuid() == 0 {
		gid, _ := strconv.Atoi(os.Getenv("SUDO_GID"))
		os.Chown(path, uid, gid)
	}
	return path, nil
}

// printQR draws text as a QR code, two rows per line, dark on light
// whatever the terminal's colors. Without colors it draws nothing.
func printQR(text string) bool {
	c, err := qr.Encode(text, qr.L)
	if err != nil || cReset == "" {
		return false
	}
	const quiet = 2
	n := c.Size + 2*quiet
	dark := func(x, y int) bool { return c.Black(x-quiet, y-quiet) }
	for y := 0; y < n; y += 2 {
		var b strings.Builder
		b.WriteString("       \033[30;107m")
		for x := 0; x < n; x++ {
			switch top, bottom := dark(x, y), dark(x, y+1); {
			case top && bottom:
				b.WriteString("█")
			case top:
				b.WriteString("▀")
			case bottom:
				b.WriteString("▄")
			default:
				b.WriteString(" ")
			}
		}
		b.WriteString(cReset)
		fmt.Println(b.String())
	}
	return true
}
