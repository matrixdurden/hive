# lyrebird

A soundboard engine that adds sounds straight to your real microphone's signal, without installing a virtual microphone (Windows). Discord, games, OBS, the browser: anything that listens to the microphone hears it.

Its interface is in [hive](../README.md): the engine runs as a library inside hive's process.

## How it works

It plugs into the microphone's Windows audio chain as an effect (APO, `apo/`). The engine carries the DLL inside itself and writes it under `Program Files\lyrebird` when connecting (needs administrator). Sounds are decoded with Media Foundation and written slightly ahead of real time into the `%ProgramData%\lyrebird\bus` shared memory; the APO inside audiodg.exe reads them from there and mixes them into the microphone signal. No allocation or locks on the real-time path.

Where it sits in the chain:

- If the driver has no stream effect (SFX): alone in the SFX slot, after all of the driver's processing. Raw streams skip SFX, so a fallback instance also sits in the endpoint chain (EFX).
- If the driver has an SFX: at the end of the EFX chain.
- On legacy drivers it takes the EFX slot and runs the driver's effect inside itself.

Installation is all-or-nothing, and removal puts everything back as it was before: the effect chains, the "disable enhancements" setting, `DisableProtectedAudioDG`, the COM and APO registrations, and the Program Files and ProgramData folders. The APO's log is `%ProgramData%\lyrebird\apo.log`, the sound list `%APPDATA%\lyrebird\ayarlar.txt`.

| Module | |
|---|---|
| `bus` | shared memory between the app and the APO (also compiled into the APO) |
| `player`, `decode` | decode sounds and write them to the shared memory |
| `monitor` | plays the same sound to your headphones |
| `hotkey` | shortcut format |
| `install` | plugs the effect into the microphones and removes it, and reports leftovers |
| `probe` | sends a test sound to every microphone and measures whether it comes back |

## Build

Cross-compiled for Windows from WSL (`x86_64-pc-windows-gnu`, needs mingw). The DLL is built first: the engine embeds it.

```sh
cargo build --release -p lyrebird-apo
cargo build --release -p lyrebird
```

`make build` at the root of the hive repository does both.

## License

MIT
