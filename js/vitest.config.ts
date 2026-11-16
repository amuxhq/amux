import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    include: ["page-review/test/**/*.test.ts", "extension/test/**/*.test.ts"],
    environment: "happy-dom",
  },
});
