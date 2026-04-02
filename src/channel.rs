use std::time::Duration;

#[cfg(target_arch = "wasm32")]
use crate::Error;

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "vzglyd_host")]
unsafe extern "C" {
    #[link_name = "channel_push"]
    fn host_channel_push(ptr: *const u8, len: i32) -> i32;
    #[link_name = "channel_poll"]
    fn host_channel_poll(ptr: *mut u8, len: i32) -> i32;
    #[link_name = "channel_active"]
    fn host_channel_active() -> i32;
    #[link_name = "log_info"]
    fn host_log_info(ptr: *const u8, len: i32) -> i32;
    #[link_name = "network_request"]
    fn host_network_request(ptr: *const u8, len: i32) -> i32;
    #[link_name = "network_response_len"]
    fn host_network_response_len() -> i32;
    #[link_name = "network_response_read"]
    fn host_network_response_read(ptr: *mut u8, len: i32) -> i32;
}

/// Push a new payload into the shared sidecar-to-slide channel.
///
/// On the WASI target this forwards to the host import. On non-WASM targets it is a no-op so
/// the crate can still be unit-tested locally.
pub fn channel_push(data: &[u8]) {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        let _ = host_channel_push(data.as_ptr(), data.len() as i32);
    }

    #[cfg(not(target_arch = "wasm32"))]
    let _ = data;
}

/// Poll the shared channel for the latest payload.
///
/// The host writes into `buf` and returns the number of bytes copied. A negative return value
/// indicates that no new payload was available or that the buffer was too small.
pub fn channel_poll(buf: &mut [u8]) -> i32 {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        return host_channel_poll(buf.as_mut_ptr(), buf.len() as i32);
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = buf;
        0
    }
}

/// Return `true` when the paired slide is currently active on screen.
pub fn channel_active() -> bool {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        return host_channel_active() != 0;
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        false
    }
}

/// Sleep for a whole number of seconds.
pub fn sleep_secs(secs: u32) {
    std::thread::sleep(Duration::from_secs(u64::from(secs)));
}

/// Emit an informational log message through the VZGLYD host.
pub fn info_log(message: &str) {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        let _ = host_log_info(message.as_ptr(), message.len() as i32);
    }

    #[cfg(not(target_arch = "wasm32"))]
    let _ = message;
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn network_roundtrip(request: &[u8]) -> Result<Vec<u8>, Error> {
    unsafe {
        let submit_status = host_network_request(request.as_ptr(), request.len() as i32);
        if submit_status != 0 {
            return Err(Error::Io(format!(
                "host network_request failed with status {submit_status}"
            )));
        }

        let response_len = host_network_response_len();
        if response_len < 0 {
            return Err(Error::Io(format!(
                "host network_response_len failed with status {response_len}"
            )));
        }

        let mut response = vec![0u8; response_len as usize];
        let read_status =
            host_network_response_read(response.as_mut_ptr(), response.len() as i32);
        if read_status < 0 {
            return Err(Error::Io(format!(
                "host network_response_read failed with status {read_status}"
            )));
        }

        response.truncate(read_status as usize);
        return Ok(response);
    }
}
