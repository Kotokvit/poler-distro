// POLER-POWERCTL — sovereign reboot / poweroff for POLER CachyOS Sovereign
// ============================================================================
// The live system has no systemd (PID 1 is poler-init) and the engine
// Terminal Gateway sandbox deliberately blocks destructive host calls
// (shutdown/halt/init/telinit). Power control therefore flows through a
// marker file in /run/poler/ which PID 1 polls:
//
//   /usr/bin/reboot    -> poler-powerctl reboot    -> /run/poler/reboot
//   /usr/bin/poweroff  -> poler-powerctl poweroff  -> /run/poler/poweroff
//
// Works from every contour of the sovereign system: the engine Terminal
// Gateway host-proxy (finds /usr/bin/reboot on PATH), poler-sh (external
// command), and scripts. Falls back to signaling PID 1 directly
// (SIGUSR1 = reboot, SIGUSR2 = poweroff) when /run is not writable —
// mirroring the poler-init signal contract.
//
// Exit codes: 0 marker accepted (or signal delivered), 1 usage error,
//             2 could not request the action by any means.
// Pure native Rust, libc only. No shell scripts.
// ============================================================================

use std::fs;

const MARKER_DIR: &str = "/run/poler";
const MARKER_REBOOT: &str = "/run/poler/reboot";
const MARKER_POWEROFF: &str = "/run/poler/poweroff";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("help");

    let (marker, label, sig) = match mode {
        "reboot" | "restart" => (MARKER_REBOOT, "reboot", libc::SIGUSR1),
        "poweroff" | "halt" | "shutdown" => (MARKER_POWEROFF, "poweroff", libc::SIGUSR2),
        _ => {
            eprintln!(
                "poler-powerctl — sovereign power control (marker IPC to poler-init PID 1)\n\
                 usage: poler-powerctl reboot | poweroff\n\
                 installed as /usr/bin/reboot and /usr/bin/poweroff in the live image"
            );
            std::process::exit(1);
        }
    };

    // Primary path: marker file consumed by the PID 1 poller.
    if let Err(e) = fs::create_dir_all(MARKER_DIR) {
        eprintln!("poler-powerctl: cannot create {}: {}", MARKER_DIR, e);
    } else if fs::write(marker, b"1\n").is_ok() {
        println!("poler-powerctl: {} requested — poler-init PID 1 will {}", label, label);
        return;
    }

    // Fallback: signal PID 1 directly (same contract as the marker path).
    unsafe {
        if libc::kill(1, sig) == 0 {
            println!("poler-powerctl: {} requested via signal to PID 1", label);
            return;
        }
    }

    eprintln!(
        "poler-powerctl: cannot request {}: no writable {} and PID 1 not signalable",
        label, MARKER_DIR
    );
    std::process::exit(2);
}
