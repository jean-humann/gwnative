//! Thread quality-of-service, which on Apple silicon means core selection —
//! and, for anything that touches the network, scheduling priority out on the
//! wire as well.
//!
//! Every `std::thread::spawn` inherits `QOS_CLASS_DEFAULT`, and the scheduler
//! reads that as "this might be interactive" — so a background sweep of 16000
//! chunks does its hashing and its decompression on performance cores, next to
//! WebContent and the GPU process, competing for the same package power budget
//! as the frame the player is waiting on.
//!
//! Saying which threads are latency-critical and which are merely throughput
//! work moves that sweep onto efficiency cores and leaves the P-cores to the
//! game.
//!
//! What the classes cannot be is a property of the thread's *job*, because the
//! same job changes meaning: the fetchers filling the cache are the launch
//! before there is a frame and background work after one, and a class chosen
//! once at spawn is wrong for half of that. [`Following`] is how a thread
//! changes its mind, and `ChunkStore::fetch_class` is where the answer lives.
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

/// A thread's class, kept in step with an answer that changes under it.
///
/// The fetch threads outlive the reason they were spawned. The same worker
/// that raced the progress bar to a first frame is still there an hour later,
/// topping the cache up behind a player who is fighting something — and the
/// right class is not the same in both halves of that life. Re-declaring costs
/// one call, and only when the answer has actually changed.
pub struct Following(u32);

impl Following {
    pub fn start(class: Class) -> Self {
        set(class);
        Self(class as u32)
    }

    /// Move to `class`, unless it is already the one in force.
    pub fn now(&mut self, class: Class) {
        if self.0 != class as u32 {
            set(class);
            self.0 = class as u32;
        }
    }
}

/// Declare the calling thread's class. Call it first thing in the thread body.
///
/// A failure is ignored on purpose. The only documented cause is a request the
/// kernel will not grant, and the consequence is that this thread keeps the
/// default class — which is exactly where it started, so there is nothing to
/// recover and nothing worth telling the player.
pub fn set(class: Class) {
    // SAFETY: both arguments are plain integers and the call touches no memory
    // of ours. `Class` is `repr(u32)` over the four `QOS_CLASS_*` constants, so
    // there is no value it can hold that the kernel has not defined.
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
