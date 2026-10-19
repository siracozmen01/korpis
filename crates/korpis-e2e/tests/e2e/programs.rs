// SPDX-License-Identifier: AGPL-3.0-or-later

//! Every program the tests run in a container, built static before any of them is wanted:
//! `scripts/check.sh` runs this first, so a test's own build of one is a check that it is fresh,
//! not minutes of compiling under the nodes the other tests are running, which would starve
//! their agents past their leases' term and lose their runs (§4.2 of docs/05-scheduling.md).

use crate::image::{EVERY, build};

#[test]
fn every_program_is_built_static() {
    for p in EVERY {
        assert!(build(p).exists(), "{} was built", p.name);
    }
}
