// SPDX-License-Identifier: AGPL-3.0-or-later

// What every test of the panel shares. The suite runs beside the whole workspace's, often on a
// loaded machine, where one second (Testing Library's default) is not always enough for a
// component to render what a mocked call returned; a test waits up to five, and fails long
// before its own timeout says nothing.

import { configure } from "@testing-library/react";

configure({ asyncUtilTimeout: 5_000 });
