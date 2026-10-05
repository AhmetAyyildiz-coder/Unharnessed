// Our icons, drawn by hand, never taken from an icon pack: 24 grid, stroke
// 1.25, round caps and joins, no fill, `currentColor`, 16 px, as few nodes as
// possible. No AI clichés: no sparkles, magic wands, robot heads or brains.
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

/** A folder: its tab on the top left. */
export function IconFolder(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M4.5 7.5A1.5 1.5 0 0 1 6 6h3.4a1 1 0 0 1 .7.3l1.6 1.6a1 1 0 0 0 .7.3H18a1.5 1.5 0 0 1 1.5 1.5v7.8A1.5 1.5 0 0 1 18 19H6a1.5 1.5 0 0 1-1.5-1.5z" />
    </Icon>
  );
}

export function IconPlus(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M12 6.5v11M6.5 12h11" />
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

export function IconChevronDown(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M7.5 10l4.5 4.5 4.5-4.5" />
    </Icon>
  );
}

/** An arrow pointing up, for send. */
export function IconSend(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M12 18.5V6M6.5 11.5L12 6l5.5 5.5" />
    </Icon>
  );
}

/** A capital T: the model reads text. */
export function IconText(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M6.5 6.5h11M12 6.5v11" />
    </Icon>
  );
}

/** A frame, a hill and a sun: the model reads images. */
export function IconImage(props: IconProps) {
  return (
    <Icon {...props}>
      <rect x="4.5" y="5.5" width="15" height="13" rx="1.5" />
      <path d="M4.5 16l4.5-4.5 4 4 2-2 4.5 4.5" />
      <circle cx="15.25" cy="9.75" r="1.25" />
    </Icon>
  );
}

/** A lightning bolt: the fast mode. */
export function IconBolt(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M13.5 4L7 13h5l-1.5 7L17 11h-5z" />
    </Icon>
  );
}

/** Two sliders, their knobs set apart: the settings. */
export function IconSliders(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M5 8.5h5M14 8.5h5M5 15.5h9M18 15.5h1" />
      <circle cx="12" cy="8.5" r="2" />
      <circle cx="16" cy="15.5" r="2" />
    </Icon>
  );
}

/** A soft square, for stop. */
export function IconStop(props: IconProps) {
  return (
    <Icon {...props}>
      <rect x="7.5" y="7.5" width="9" height="9" rx="2" />
    </Icon>
  );
}

/** A speech bubble with a plus: a new conversation. */
export function IconNewChat(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M6 5.5h12a1.5 1.5 0 0 1 1.5 1.5v8a1.5 1.5 0 0 1-1.5 1.5h-7.5L7 19.5v-3H6A1.5 1.5 0 0 1 4.5 15V7A1.5 1.5 0 0 1 6 5.5z" />
      <path d="M12 8.5v5M9.5 11h5" />
    </Icon>
  );
}

/** A prompt: a chevron and a line, for a cell the agent runs. */
export function IconCell(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M6 8l4 4-4 4M12.5 16.5H18" />
    </Icon>
  );
}

/** A paper clip: attach an image. */
export function IconClip(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M15.5 8.5l-6 6a1.77 1.77 0 0 0 2.5 2.5l6.5-6.5a3.54 3.54 0 0 0-5-5L7 12a5.3 5.3 0 0 0 7.5 7.5L19 15" />
    </Icon>
  );
}

/** An arrow pointing down, to the latest message. */
export function IconArrowDown(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M12 6v12.5M6.5 13L12 18.5l5.5-5.5" />
    </Icon>
  );
}
