//! Thread quality-of-service, which on Apple silicon means core selection.
//!
//! Every `std::thread::spawn` inherits `QOS_CLASS_DEFAULT`, and the scheduler
//! reads that as "this might be interactive" — so a background sweep of 16000
//! chunks does its hashing and its decompression on performance cores, next to
//! WebContent and the GPU process, competing for the same package power budget
//! as the frame the player is waiting on.
//!
//! Saying which threads are latency-critical and which are merely throughput
//! work moves the sweep onto efficiency cores and leaves the P-cores to the
//! game. It is also something Electron structurally cannot do: its equivalent
//! work runs on libuv's threadpool at whatever QoS the pool was created with.
//!
//! [`ChunkStore`](crate::chunks::ChunkStore) already reasons about this at the
//! HTTP-concurrency level — `MAX_PREFETCH_FETCHES` reserves request slots for
//! demand reads — but that only rations the network. This rations the CPU.

/// The classes we use, with the values from `<pthread/qos.h>`.
///
/// Not the whole enum: `USER_INTERACTIVE` belongs to the thread driving the
/// screen, which in this process is AppKit's main thread and already has it,
/// and `BACKGROUND` is for work that may be suspended indefinitely — a prefetch
/// the player is watching a progress bar for is not that.
#[derive(Clone, Copy)]
#[repr(u32)]
pub enum Class {
    /// The player is blocked on this: a range read, a socket, an accept.
    UserInitiated = 0x19,
    /// Useful work nobody is waiting on. Efficiency cores, lower power.
    Utility = 0x11,
}

#[link(name = "System", kind = "dylib")]
unsafe extern "C" {
    fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
}

/// Declare the calling thread's class. Call it first thing in the thread body.
///
/// A failure is ignored on purpose. The only documented cause is a request the
/// kernel will not grant, and the consequence is that this thread keeps the
/// default class — which is exactly where it started, so there is nothing to
/// recover and nothing worth telling the player.
pub fn set(class: Class) {
    unsafe {
        pthread_set_qos_class_self_np(class as u32, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[link(name = "System", kind = "dylib")]
    unsafe extern "C" {
        fn pthread_get_qos_class_np(
            thread: *mut std::ffi::c_void,
            qos_class: *mut u32,
            relative_priority: *mut i32,
        ) -> i32;
        fn pthread_self() -> *mut std::ffi::c_void;
    }

    fn current() -> u32 {
        let mut class = 0u32;
        let mut priority = 0i32;
        unsafe {
            assert_eq!(
                pthread_get_qos_class_np(pthread_self(), &mut class, &mut priority),
                0
            );
        }
        class
    }

    /// The point of the module is that the kernel actually records the class —
    /// a constant that silently fails to apply would look identical from here.
    #[test]
    fn the_kernel_takes_the_class_it_is_given() {
        // On its own thread: this changes the caller, and the test harness's
        // threads are shared with whatever runs next.
        std::thread::spawn(|| {
            assert_ne!(current(), Class::Utility as u32, "default is not utility");
            set(Class::Utility);
            assert_eq!(current(), Class::Utility as u32);
            set(Class::UserInitiated);
            assert_eq!(current(), Class::UserInitiated as u32);
        })
        .join()
        .unwrap();
    }
}
