//! Installing demirror's display driver on Windows, which lets monitors extend the desktop.
//!
//! The driver's files sit in `driver\` next to demirror.exe. Installing it needs
//! administrator rights, so the app runs itself elevated (`demirror --install-driver`, and
//! Windows asks the user first); the installer runs the same command.

use std::{
    mem,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
    ptr,
};

use anyhow::{Context, Result, bail, ensure};
use windows_sys::{
    Win32::{
        Devices::DeviceAndDriverInstallation::*,
        Foundation::{CloseHandle, ERROR_CANCELLED, GetLastError, WAIT_OBJECT_0},
        System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject},
        UI::{Shell::*, WindowsAndMessaging::SW_HIDE},
    },
    core::GUID,
};

/// The driver's device, as the INF names it.
const HARDWARE_ID: &str = r"Root\demirror_idd";
/// The subject of the certificate a self-signed driver comes with (build.ps1).
const CERTIFICATE: &str = "demirror display driver";
const NO_WINDOW: u32 = 0x0800_0000;

fn folder() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let folder = exe.parent().context("no folder for demirror.exe")?.join("driver");
    ensure!(folder.join("demirror_idd.inf").exists(), "the driver's files aren't in {}", folder.display());
    Ok(folder)
}

/// Whether the driver's files came with this copy of demirror.
pub fn available() -> bool {
    folder().is_ok()
}

/// Installs the driver as administrator (Windows asks the user) and waits until it's done.
pub fn install_elevated() -> Result<()> {
    let log = std::env::temp_dir().join("demirror-driver.log");
    let _ = std::fs::remove_file(&log);
    let exe = wide(&std::env::current_exe()?.to_string_lossy());
    let parameters = wide(&format!("--install-driver --log \"{}\"", log.display()));
    let verb = wide("runas");
    unsafe {
        let mut run: SHELLEXECUTEINFOW = mem::zeroed();
        run.cbSize = mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        run.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
        run.lpVerb = verb.as_ptr();
        run.lpFile = exe.as_ptr();
        run.lpParameters = parameters.as_ptr();
        run.nShow = SW_HIDE;
        if ShellExecuteExW(&mut run) == 0 {
            if GetLastError() == ERROR_CANCELLED {
                bail!("Windows didn't get permission to install the driver");
            }
            bail!("couldn't start the installation: {}", std::io::Error::last_os_error());
        }
        let mut code = 1u32;
        if WaitForSingleObject(run.hProcess, INFINITE) == WAIT_OBJECT_0 {
            GetExitCodeProcess(run.hProcess, &mut code);
        }
        CloseHandle(run.hProcess);
        if code != 0 {
            let reason = std::fs::read_to_string(&log).unwrap_or_default();
            bail!("installing the driver failed: {}", reason.trim());
        }
    }
    Ok(())
}

/// `--install-driver` and `--uninstall-driver` (with `--log FILE` for the error): runs it and
/// returns the exit code, or `None` for any other command line.
pub fn run_command(args: &[String]) -> Option<i32> {
    let result = if args.iter().any(|a| a == "--install-driver") {
        install()
    } else if args.iter().any(|a| a == "--uninstall-driver") {
        uninstall()
    } else {
        return None;
    };
    Some(match result {
        Ok(()) => 0,
        Err(e) => {
            let log = args.iter().position(|a| a == "--log").and_then(|i| args.get(i + 1));
            if let Some(log) = log {
                let _ = std::fs::write(log, format!("{e:#}"));
            }
            eprintln!("{e:#}");
            1
        }
    })
}

fn install() -> Result<()> {
    let folder = folder()?;
    // A self-signed driver is trusted on this computer only through its own certificate.
    let certificate = folder.join("demirror.cer");
    if certificate.exists() {
        for store in ["Root", "TrustedPublisher"] {
            certutil(&["-f", "-addstore", store, &certificate.to_string_lossy()])
                .with_context(|| format!("trusting the driver's certificate ({store})"))?;
        }
    }
    let inf = folder.join("demirror_idd.inf");
    if devices(|_, _| {})? == 0 {
        create_device(&inf)?;
    }
    let mut reboot = 0;
    let (id, inf) = (wide(HARDWARE_ID), wide(&inf.to_string_lossy()));
    if unsafe { UpdateDriverForPlugAndPlayDevicesW(ptr::null_mut(), id.as_ptr(), inf.as_ptr(), INSTALLFLAG_FORCE, &mut reboot) } == 0 {
        bail!("Windows didn't install the driver: {}", std::io::Error::last_os_error());
    }
    Ok(())
}

fn uninstall() -> Result<()> {
    devices(|set, device| unsafe {
        let mut reboot = 0;
        DiUninstallDevice(ptr::null_mut(), set, device, 0, &mut reboot);
    })?;
    if let Ok(folder) = folder() {
        let inf = wide(&folder.join("demirror_idd.inf").to_string_lossy());
        let mut reboot = 0;
        unsafe { DiUninstallDriverW(ptr::null_mut(), inf.as_ptr(), 0, &mut reboot) };
    }
    for store in ["Root", "TrustedPublisher"] {
        let _ = certutil(&["-delstore", store, CERTIFICATE]);
    }
    Ok(())
}

/// A root device for the driver to run on (what `devcon install` does).
fn create_device(inf: &Path) -> Result<()> {
    let inf = wide(&inf.to_string_lossy());
    unsafe {
        let mut class: GUID = mem::zeroed();
        let mut class_name = [0u16; 32];
        if SetupDiGetINFClassW(inf.as_ptr(), &mut class, class_name.as_mut_ptr(), class_name.len() as u32, ptr::null_mut()) == 0 {
            bail!("reading the driver's INF: {}", std::io::Error::last_os_error());
        }
        let set = SetupDiCreateDeviceInfoList(&class, ptr::null_mut());
        ensure!(set != -1, "can't add a device: {}", std::io::Error::last_os_error());
        let result = (|| {
            let mut device: SP_DEVINFO_DATA = mem::zeroed();
            device.cbSize = mem::size_of::<SP_DEVINFO_DATA>() as u32;
            if SetupDiCreateDeviceInfoW(set, class_name.as_ptr(), &class, ptr::null(), ptr::null_mut(), DICD_GENERATE_ID, &mut device) == 0 {
                bail!("can't add a device: {}", std::io::Error::last_os_error());
            }
            let ids: Vec<u16> = HARDWARE_ID.encode_utf16().chain([0, 0]).collect();
            let size = (ids.len() * 2) as u32;
            if SetupDiSetDeviceRegistryPropertyW(set, &mut device, SPDRP_HARDWAREID, ids.as_ptr().cast(), size) == 0 {
                bail!("can't name the device: {}", std::io::Error::last_os_error());
            }
            if SetupDiCallClassInstaller(DIF_REGISTERDEVICE, set, &device) == 0 {
                bail!("can't register the device: {}", std::io::Error::last_os_error());
            }
            Ok(())
        })();
        SetupDiDestroyDeviceInfoList(set);
        result
    }
}

/// Calls `found` with each of the driver's devices and returns how many there are.
fn devices(mut found: impl FnMut(HDEVINFO, &SP_DEVINFO_DATA)) -> Result<usize> {
    let root = wide("ROOT");
    unsafe {
        let set = SetupDiGetClassDevsW(ptr::null(), root.as_ptr(), ptr::null_mut(), DIGCF_ALLCLASSES);
        ensure!(set != -1, "can't list devices: {}", std::io::Error::last_os_error());
        let mut count = 0;
        let mut device: SP_DEVINFO_DATA = mem::zeroed();
        device.cbSize = mem::size_of::<SP_DEVINFO_DATA>() as u32;
        let mut index = 0;
        while SetupDiEnumDeviceInfo(set, index, &mut device) != 0 {
            index += 1;
            let mut ids = [0u16; 512];
            let size = mem::size_of_val(&ids) as u32;
            let read = SetupDiGetDeviceRegistryPropertyW(set, &device, SPDRP_HARDWAREID, ptr::null_mut(), ids.as_mut_ptr().cast(), size, ptr::null_mut());
            if read != 0 && String::from_utf16_lossy(&ids).split('\0').any(|id| id.eq_ignore_ascii_case(HARDWARE_ID)) {
                count += 1;
                found(set, &device);
            }
        }
        SetupDiDestroyDeviceInfoList(set);
        Ok(count)
    }
}

fn certutil(args: &[&str]) -> Result<()> {
    let output = Command::new("certutil").args(args).creation_flags(NO_WINDOW).output()?;
    ensure!(output.status.success(), "certutil: {}", String::from_utf8_lossy(&output.stdout).trim());
    Ok(())
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}
