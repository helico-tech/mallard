import { applyDiscount } from "./pricing";

export interface LineItem {
  sku: string;
  price: number;
  qty: number;
}

export interface Cart {
  items: LineItem[];
  coupon: string;
}

export function computeTotal(cart: Cart): number {
  let subtotal = 0;
  for (const item of cart.items) {
    subtotal += item.price * item.qty;
  }
  return applyDiscount(subtotal, cart.coupon);
}
