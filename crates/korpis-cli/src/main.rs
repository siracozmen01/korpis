// SPDX-License-Identifier: AGPL-3.0-or-later

#[expect(clippy::print_stdout, reason = "until it is implemented, the version line is all it says")]
fn main() {
    println!("{}", korpis_core::build::describe("korpis"));
}
