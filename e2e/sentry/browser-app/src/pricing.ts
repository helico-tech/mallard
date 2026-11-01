interface Discount {
  rate: number;
  label: string;
}

const DISCOUNTS: Record<string, Discount> = {
  SPRING: { rate: 0.1, label: "Spring sale" },
};

export function applyDiscount(subtotal: number, coupon: string): number {
  const discount = DISCOUNTS[coupon];
  if (subtotal <= 0) {
    return 0;
  }
  // Bug: unknown coupons have no discount entry.
  const rate = (discount as Discount).rate.toFixed(2);
  return Math.round(subtotal * (1 - Number(rate)));
}
