/** A mallard's head: the mark of the app, in the design system's accent. */
export function Logo({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 32 32" className={className} aria-hidden>
      <rect width="32" height="32" rx="7" fill="var(--text-color-kumo-brand)" />
      <path fill="#fff" d="M18.6 7.5a5.6 5.6 0 0 0-5.6 5.6c0 1 .3 2 .7 2.800-2 .6-4 .2-6.100-1.100-.8-.5-1.800.2-1.600 1.100.9 4.800 4.400 8.600 9.700 8.600 4.900 0 8.100-3.400 8.100-7.400 0-1.500-.5-2.900-1.300-4.100l3.900-1.500-3.600-1.400a5.600 5.600 0 0 0-4.200-2.600z" />
      <circle cx="19" cy="12.500" r="1.200" fill="var(--text-color-kumo-brand)" />
    </svg>
  );
}
