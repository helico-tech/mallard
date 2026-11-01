import { describe, expect, it } from "vitest";
import { serviceParam } from "./scope";

describe("serviceParam", () => {
  it("All apps: only the filter limits", () => {
    expect(serviceParam(undefined, null)).toEqual({});
    expect(serviceParam("api", null)).toEqual({ service: ["api"] });
  });
  it("an app sends all its names, and a filter narrows them", () => {
    expect(serviceParam(undefined, ["Checkout API", "checkout-api"])).toEqual({ service: ["Checkout API", "checkout-api"] });
    expect(serviceParam("checkout-api", ["Checkout API", "checkout-api"])).toEqual({ service: ["checkout-api"] });
  });
  it("a service outside the app matches nothing, and reads nothing", () => {
    expect(serviceParam("payments", ["web"])).toBeNull();
  });
});
