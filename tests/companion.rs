//! Execute the freestanding companion against a deterministic client-memory
//! fixture, then let the production JavaScript decoder read its seqlock page.

use std::path::Path;
use std::process::Command;

#[test]
fn companion_kernel_publishes_agents_quests_inventory_and_social_end_to_end() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/test_companion_kernel.mjs");
    let output = match Command::new("node")
        .arg(script)
        .env(
            "GWNATIVE_COMPANION_KERNEL",
            env!("GWNATIVE_COMPANION_KERNEL"),
        )
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("[companion] skipped: node is not installed");
            return;
        }
        Err(error) => panic!("could not run companion fixture: {error}"),
    };
    if !output.status.success() {
        panic!(
            "companion fixture failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}
