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

export function IconBack(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M14.5 6.5L9 12l5.5 5.5" />
    </Icon>
  );
}

export function IconChevron(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M9.5 6.5L15 12l-5.5 5.5" />
    </Icon>
  );
}

/** A frame left open at the top right, an arrow leaving through the gap. */
export function IconExternal(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M13.5 5.5h5v5M18.5 5.5l-7 7" />
      <path d="M16.5 13.5v4a1 1 0 0 1-1 1h-9a1 1 0 0 1-1-1v-9a1 1 0 0 1 1-1h4" />
    </Icon>
  );
}

/** Two sheets, the back one showing only its corner. */
export function IconCopy(props: IconProps) {
  return (
    <Icon {...props}>
      <rect x="9" y="9" width="9.5" height="9.5" rx="1.5" />
      <path d="M15 9V6.5a1 1 0 0 0-1-1H6.5a1 1 0 0 0-1 1V14a1 1 0 0 0 1 1H9" />
    </Icon>
  );
}

export function IconCheck(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M6.5 12.5l3.5 3.5 7.5-8" />
    </Icon>
  );
}

/** A chain link with its middle bar gone, two ticks marking the break. */
export function IconUnlink(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M10 8H8a4 4 0 0 0 0 8h2M14 8h2a4 4 0 0 1 0 8h-2" />
      <path d="M12 4.5v1.5M12 18v1.5" />
    </Icon>
  );
}

export function IconSearch(props: IconProps) {
  return (
    <Icon {...props}>
      <circle cx="11" cy="11" r="5.5" />
      <path d="M15.5 15.5L19 19" />
    </Icon>
  );
}

/** A plug: two prongs, a rounded body, the cord. */
export function IconPlug(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M9.5 4.5v4M14.5 4.5v4" />
      <path d="M7 8.5h10V11a5 5 0 0 1-10 0z" />
      <path d="M12 16v3.5" />
    </Icon>
  );
}
