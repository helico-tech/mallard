import { computeTotal, type Cart } from "./cart";

export interface Order {
  id: string;
  total: number;
}

export function submitOrder(cart: Cart): Order {
  const total = computeTotal(cart);
  return { id: `ord-${Date.now()}`, total };
}
