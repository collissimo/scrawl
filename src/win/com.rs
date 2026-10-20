//! COM and Media Foundation lifetimes for worker threads (§3 rule 8).

use win32::Win32::Foundation::RPC_E_CHANGED_MODE;
use win32::Win32::Media::MediaFoundation::{MF_VERSION, MFSTARTUP_NOSOCKET, MFShutdown, MFStartup};
use win32::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

/// COM initialised for this thread in the multithreaded apartment. A thread that already
/// joined an apartment (`RPC_E_CHANGED_MODE`) keeps it and is not uninitialised on drop.
pub struct ComGuard {
    initialized: bool,
    _not_send: std::marker::PhantomData<*const ()>,
}

impl ComGuard {
    pub fn mta() -> Self {
        // SAFETY: called on the current thread; balanced in Drop when it succeeded.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        // S_OK and S_FALSE both take a reference that CoUninitialize must release.
        let initialized = hr.is_ok();
        if !initialized && hr != RPC_E_CHANGED_MODE {
            crate::capture::scrawl_log(format!("CoInitializeEx failed: {hr:?}"));
        }
        Self {
            initialized,
            _not_send: std::marker::PhantomData,
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: balances the successful CoInitializeEx on this thread.
            unsafe { CoUninitialize() };
        }
    }
}

/// Media Foundation started for as long as this lives.
pub struct MfGuard {
    _private: (),
}

impl MfGuard {
    pub fn start() -> anyhow::Result<Self> {
        // SAFETY: MFStartup has no preconditions; balanced by MFShutdown in Drop.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) }
            .map_err(|error| anyhow::anyhow!("MFStartup failed: {error}"))?;
        Ok(Self { _private: () })
    }
}

impl Drop for MfGuard {
    fn drop(&mut self) {
        // SAFETY: balances the successful MFStartup.
        let _ = unsafe { MFShutdown() };
    }
}
