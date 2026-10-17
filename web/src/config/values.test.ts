// SPDX-License-Identifier: AGPL-3.0-or-later

import { create } from "@bufbuild/protobuf";
import { expect, test } from "vitest";
import { ConfigFieldSchema, ConfigType } from "../gen/korpis/v1/recipe_pb";
import { plain, read, shown } from "./values";

test("a recipe's message reads as its quoting says", () => {
  expect(plain("It''s '{here}'")).toBe("It's {here}");
  expect(plain("don't")).toBe("don't");
});

test("a value is read back as its field's type, or refused", () => {
  const integer = create(ConfigFieldSchema, { key: "n", type: ConfigType.INTEGER });
  expect(shown(read(integer, "42") as never)).toBe("42");
  expect(read(integer, "4.2")).toBe("invalid");
  expect(read(integer, "  ")).toBeUndefined();
  const text = create(ConfigFieldSchema, { key: "s", type: ConfigType.STRING });
  expect(shown(read(text, "hi there") as never)).toBe("hi there");
});
