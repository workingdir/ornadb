import { expect, test } from "@playwright/test";

const sampleResults = [
  { id: "arithmetic", values: ["42 : Int"] },
  { id: "multiline", values: ["42 : Int", "43 : Int"] },
  { id: "values", values: ["true : Bool", '"Orna" : Str'] },
];

test("the browser Run action evaluates shared sample programs through WASM", async ({ page }) => {
  const pageErrors = [];
  page.on("pageerror", (error) => pageErrors.push(error.message));

  await page.goto("./");
  await expect(page.locator(".runtime-status")).toContainText("WASM ready");

  for (const sample of sampleResults) {
    await page.getByLabel("Sample").selectOption(sample.id);
    await page.getByRole("button", { name: /^Run/ }).click();
    await expect(page.locator("#output .output-row.value pre")).toHaveText(sample.values);
    await expect(page.locator("#output .output-row.error")).toHaveCount(0);
  }

  expect(pageErrors).toEqual([]);
});
