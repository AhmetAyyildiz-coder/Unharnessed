// Hand-drawn icons (DESIGN.md): 24 grid, stroke 1.25, round caps, no fill.
import type { SVGProps } from "react";

type IconProps = SVGProps<SVGSVGElement>;

function Icon({ children, ...props }: IconProps) {
  return (
    <svg
      viewBox="0 0 24 24"
      width={16}
      height={16}
      fill="none"
      stroke="currentColor"
      strokeWidth={1.25}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      {...props}
    >
      {children}
    </svg>
  );
}

/** A circle closing clockwise from 3 o'clock, arrowhead at the gap. */
export function IconRestart(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M18.5 12A6.5 6.5 0 1 1 16.6 7.4" />
      <path d="M16.08 4.45L16.6 7.4L13.65 6.88" />
    </Icon>
  );
}

export function IconCross(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M8.5 8.5l7 7M15.5 8.5l-7 7" />
    </Icon>
  );
}
