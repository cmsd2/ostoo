//! Handle `CloseResult` from fd close operations.
//!
//! Translates libkernel's close actions into concrete operations (scheduler
//! donate, completion port posting with errno values).

use libkernel::completion_port::{Completion, OP_IPC_RECV, OP_IPC_SEND};
use libkernel::file::CloseResult;
use libkernel::task::scheduler;

use crate::errno;

/// Handle a `CloseResult` returned by `FdObject::close()` / `Process::close_fd()`.
pub fn handle_close_result(result: CloseResult) {
    match result {
        CloseResult::None => {}
        CloseResult::WakeThread(thread_idx) => {
            scheduler::set_donate_target(thread_idx);
            scheduler::yield_now();
        }
        CloseResult::NotifyRecvPort(pr) => {
            let woken = pr.port.lock().post(Completion {
                user_data: pr.user_data,
                result: -errno::EPIPE,
                flags: 0,
                opcode: OP_IPC_RECV,
                read_buf: None,
                read_dest: 0,
            });
            if let Some(thread_idx) = woken {
                scheduler::set_donate_target(thread_idx);
                scheduler::yield_now();
            }
        }
        CloseResult::NotifySendPort(ps) => {
            let woken = ps.port.lock().post(Completion {
                user_data: ps.user_data,
                result: -errno::EPIPE,
                flags: 0,
                opcode: OP_IPC_SEND,
                read_buf: None,
                read_dest: 0,
            });
            if let Some(thread_idx) = woken {
                scheduler::set_donate_target(thread_idx);
                scheduler::yield_now();
            }
        }
    }
}
