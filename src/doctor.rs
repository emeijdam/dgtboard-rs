//! `dgt doctor`: diagnose why a board won't connect.
//!
//! DGT serial e-boards use an FTDI USB-serial chip, but with DGT's *custom*
//! USB vendor id — so macOS's built-in FTDI support doesn't bind to it and you
//! must install the FTDI VCP driver (a DriverKit system extension) and then
//! approve it in System Settings. This command checks each link in that chain
//! and tells you the next step.

use std::path::Path;
use std::process::Command;

use serialport::SerialPortType;

/// DGT's reported USB vendor id and a board product id we've seen.
const DGT_VID: u16 = 0x045b;
const DGT_VID_DECIMAL: &str = "1115"; // how ioreg prints it

/// Substrings that identify a USB-serial driver bundle id.
const SERIAL_DRIVER_HINTS: &[&str] = &[
    "ftdi", "silabs", "cp210", "wch", "ch34", "prolific", "pl2303", "usbserial", "vcp",
];

struct UsbDevice {
    name: String,
    vid: Option<u16>,
    pid: Option<u16>,
}

struct DriverExt {
    bundle: String,
    enabled: bool,
    state: String,
}

impl DriverExt {
    fn is_serial(&self) -> bool {
        let b = self.bundle.to_ascii_lowercase();
        SERIAL_DRIVER_HINTS.iter().any(|h| b.contains(h))
    }
    fn waiting_for_user(&self) -> bool {
        self.state.to_ascii_lowercase().contains("waiting for user")
    }
}

/// Run a command and return stdout (or non-empty stderr) on a clean-ish exit.
fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !stdout.trim().is_empty() {
        return Some(stdout.into_owned());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !stderr.trim().is_empty() {
        Some(stderr.into_owned())
    } else {
        None
    }
}

/// Look for the DGT board in the live USB device tree (macOS `ioreg`).
fn detect_usb() -> Option<UsbDevice> {
    let dump = ["IOUSBHostDevice", "IOUSBDevice"]
        .iter()
        .find_map(|class| run("ioreg", &["-r", "-c", class, "-l", "-w", "0"]))?;

    let is_dgt = dump.contains("DGT") || dump.contains(&format!("\"idVendor\" = {DGT_VID_DECIMAL}"));
    if !is_dgt {
        return None;
    }

    // Pull the first product name and the vid/pid near the DGT entry.
    let name = dump
        .lines()
        .find(|l| l.contains("Product Name") && l.contains("DGT"))
        .and_then(|l| l.split('"').nth(3))
        .unwrap_or("DGT e-Board")
        .to_string();
    let parse_after = |key: &str| -> Option<u16> {
        dump.lines()
            .find(|l| l.contains(key))
            .and_then(|l| l.rsplit('=').next())
            .and_then(|v| v.trim().parse::<u32>().ok())
            .map(|v| v as u16)
    };
    Some(UsbDevice {
        name,
        vid: parse_after("\"idVendor\""),
        pid: parse_after("\"idProduct\""),
    })
}

/// USB serial ports, with the DGT one (if any) first.
fn detect_ports() -> (Option<String>, Vec<String>) {
    let ports = serialport::available_ports().unwrap_or_default();
    let mut dgt = None;
    let mut others = Vec::new();
    for p in ports {
        if let SerialPortType::UsbPort(info) = &p.port_type {
            let looks_dgt = info.vid == DGT_VID
                || info
                    .product
                    .as_deref()
                    .map(|s| s.to_ascii_lowercase().contains("dgt"))
                    .unwrap_or(false);
            if looks_dgt && (dgt.is_none() || p.port_name.contains("cu.")) {
                dgt = Some(p.port_name.clone());
            } else {
                others.push(p.port_name.clone());
            }
        }
    }
    (dgt, others)
}

/// Parse `systemextensionsctl list` into driver extensions + the OS's own hint
/// about where to manage them.
fn detect_driver_extensions() -> (Vec<DriverExt>, Option<String>) {
    let Some(out) = run("systemextensionsctl", &["list"]) else {
        return (Vec::new(), None);
    };
    let hint = out
        .lines()
        .find(|l| l.contains("Go to"))
        .and_then(|l| l.split_once("Go to "))
        .map(|(_, rest)| rest.trim_end_matches([')', ' ']).to_string());

    let mut exts = Vec::new();
    for line in out.lines() {
        // Data rows have a bracketed [state] and tab-separated columns.
        if !line.contains('[') || !line.contains(']') {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 6 {
            continue;
        }
        let bundle = cols[3]
            .split_whitespace()
            .next()
            .unwrap_or(cols[3])
            .to_string();
        let state = cols[5].trim().trim_matches(['[', ']']).to_string();
        exts.push(DriverExt {
            bundle,
            enabled: cols[0].contains('*'),
            state,
        });
    }
    (exts, hint)
}

/// Legacy kext-based serial drivers installed on disk (pre-DriverKit).
fn detect_legacy_kexts() -> Vec<String> {
    const KEXTS: &[&str] = &[
        "/Library/Extensions/FTDIUSBSerialDriver.kext",
        "/Library/Extensions/SiLabsUSBDriver.kext",
        "/Library/Extensions/usbserial.kext",
        "/Library/Extensions/ProlificUsbSerial.kext",
    ];
    KEXTS
        .iter()
        .filter(|p| Path::new(p).exists())
        .map(|p| p.to_string())
        .collect()
}

fn open_security_settings() {
    // Best-effort deep links; if one is stale the pane still opens to a sane
    // place. macOS 15 moved this under General; 13–14 under Privacy & Security.
    for url in [
        "x-apple.systempreferences:com.apple.LoginItems-Settings.extension",
        "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension",
    ] {
        if Command::new("open").arg(url).status().map(|s| s.success()).unwrap_or(false) {
            return;
        }
    }
    let _ = Command::new("open").arg("/System/Applications/System Settings.app").status();
}

const FTDI_VCP_URL: &str = "https://ftdichip.com/drivers/vcp-drivers/";

/// Run the full diagnosis. Returns `true` if the board looks ready to use.
pub fn run_doctor(open_settings: bool) -> bool {
    println!("DGT board connection check");
    println!("==========================\n");

    if !cfg!(target_os = "macos") {
        println!("Note: detailed driver/security checks are macOS-specific.");
        println!("On Linux the board appears as /dev/ttyUSB* (you may need to be in the");
        println!("`dialout` group); on Windows it's a COM port via the FTDI VCP driver.\n");
    }

    let usb = detect_usb();
    let (port, other_ports) = detect_ports();
    let (exts, manage_hint) = detect_driver_extensions();
    let legacy = detect_legacy_kexts();
    let serial_drivers: Vec<&DriverExt> = exts.iter().filter(|e| e.is_serial()).collect();
    let waiting = serial_drivers.iter().any(|e| e.waiting_for_user());

    // 1) USB presence
    match &usb {
        Some(d) => {
            let ids = match (d.vid, d.pid) {
                (Some(v), Some(p)) => format!(" (VID {v:04x} PID {p:04x})"),
                _ => String::new(),
            };
            println!("[1/4] USB device   ✓  Board detected: {}{}", d.name, ids);
        }
        None => {
            println!("[1/4] USB device   ✗  No DGT board found on USB.");
            println!("      → Use a DATA-capable USB-C cable (charge-only cables don't carry data).");
            println!("      → Reseat the connector and try a different port/hub.");
        }
    }

    // 2) Serial port node
    match &port {
        Some(p) => println!("[2/4] Serial port  ✓  {p}"),
        None => {
            println!("[2/4] Serial port  ✗  No serial port for the board yet.");
            if !other_ports.is_empty() {
                println!("      (other USB serial ports present: {})", other_ports.join(", "));
            }
        }
    }

    // 3) Driver
    if serial_drivers.is_empty() && legacy.is_empty() {
        println!("[3/4] Driver       ✗  No USB-serial driver installed.");
        println!("      DGT boards use an FTDI chip with a custom vendor id, so macOS's");
        println!("      built-in FTDI support won't bind. Install the FTDI VCP driver:");
        println!("      → {FTDI_VCP_URL}");
    } else {
        for e in &serial_drivers {
            let mark = if e.enabled && !e.waiting_for_user() { "✓" } else { "⚠" };
            println!("[3/4] Driver       {mark}  {} [{}]", e.bundle, e.state);
        }
        for k in &legacy {
            println!("[3/4] Driver       ✓  {k}");
        }
    }

    // 4) Security / approval
    if waiting {
        println!("[4/4] Security     ⚠  Driver installed but NOT approved.");
        approval_steps(manage_hint.as_deref());
    } else if !serial_drivers.is_empty() || !legacy.is_empty() {
        println!("[4/4] Security     ✓  Driver approved.");
    } else {
        println!("[4/4] Security     —  (install a driver first; you'll then approve it here)");
        approval_steps(manage_hint.as_deref());
    }

    // Verdict
    println!("\n--------------------------------------------------");
    let ready = port.is_some();
    if ready {
        println!("✓ Ready. The board is connected. Try:  dgt snapshot");
    } else if usb.is_none() {
        println!("✗ Board not detected. Check the cable (must be a data cable) and the port.");
    } else if waiting {
        println!("✗ Driver is installed but blocked. Approve it (step 4), then UNPLUG and");
        println!("  replug the board. Re-run `dgt doctor` to confirm.");
    } else if serial_drivers.is_empty() && legacy.is_empty() {
        println!("✗ Board is on USB but has no serial driver.");
        println!("  1. Install the FTDI VCP driver: {FTDI_VCP_URL}");
        println!("  2. Approve it in System Settings (step 4 above).");
        println!("  3. Unplug and replug the board, then re-run `dgt doctor`.");
    } else {
        println!("⚠ Driver is installed and approved, but no port appeared.");
        println!("  Unplug/replug the board (or reboot once), then re-run `dgt doctor`.");
    }

    if open_settings {
        println!("\nOpening System Settings…");
        open_security_settings();
    } else if !ready {
        println!("\nTip: run `dgt doctor --open-settings` to jump straight to the approval screen.");
    }

    ready
}

fn approval_steps(manage_hint: Option<&str>) {
    if let Some(hint) = manage_hint {
        // The OS told us exactly where; surface it verbatim.
        println!("      → {hint}");
    } else {
        println!("      → macOS 15+: System Settings → General → Login Items & Extensions");
        println!("                   → Driver Extensions → enable the FTDI driver");
        println!("      → macOS 13–14: System Settings → Privacy & Security → scroll down");
        println!("                   → \"Allow\" the blocked system software");
    }
    println!("      Then UNPLUG and replug the board.");
}
