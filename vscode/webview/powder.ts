// The press: a pressed control gives a little, springs
// back, and a puff of cyan dust leaves from under the pointer.
import { reducedMotion } from "./motion";

const SPRING = "cubic-bezier(0.34, 1.56, 0.64, 1)";
const SOFT = "cubic-bezier(0.22, 1, 0.36, 1)";
const GRAINS = 9;

const pressing = new WeakMap<Element, Animation>();

document.addEventListener("pointerdown", (event) => {
  const control = (event.target as Element | null)?.closest<HTMLElement>("button:not(:disabled)");
  if (!control || event.button !== 0) return;
  // Wide rows barely give; small pills and discs give more.
  const depth = control.offsetWidth > 120 ? 0.985 : 0.92;
  pressing.get(control)?.cancel();
  pressing.set(
    control,
    control.animate([{ scale: "1" }, { scale: String(depth) }], { duration: 110, easing: "ease-out", fill: "forwards" }),
  );
  const release = () => {
    control.removeEventListener("pointerup", release);
    control.removeEventListener("pointerleave", release);
    pressing.get(control)?.cancel();
    pressing.set(
      control,
      control.animate([{ scale: String(depth) }, { scale: "1.03", offset: 0.55 }, { scale: "1" }], {
        duration: reducedMotion() ? 1 : 460,
        easing: SPRING,
      }),
    );
  };
  control.addEventListener("pointerup", release);
  control.addEventListener("pointerleave", release);
  if (!reducedMotion()) dust(event.clientX, event.clientY);
});

function dust(x: number, y: number) {
  const style = getComputedStyle(document.body);
  const accent = style.getPropertyValue("--uh-accent-rgb").trim();
  const ink = style.getPropertyValue("--uh-ink-rgb").trim();

  const puff = document.createElement("span");
  puff.className = "puff";
  puff.style.left = `${x}px`;
  puff.style.top = `${y}px`;
  document.body.append(puff);
  puff
    .animate([{ transform: "scale(0.2)", opacity: 0.95 }, { transform: "scale(2.2)", opacity: 0 }], {
      duration: 700,
      easing: SOFT,
    })
    .finished.then(() => puff.remove(), () => puff.remove());

  for (let i = 0; i < GRAINS; i++) {
    const grain = document.createElement("span");
    const size = 1.5 + Math.random() * 2;
    const angle = Math.random() * Math.PI * 2;
    const distance = 12 + Math.random() * 20;
    grain.className = "grain";
    grain.style.left = `${x - size / 2}px`;
    grain.style.top = `${y - size / 2}px`;
    grain.style.width = grain.style.height = `${size}px`;
    grain.style.background = `rgb(${i % 3 === 0 ? ink : accent} / 0.85)`;
    document.body.append(grain);
    grain
      .animate(
        [
          { transform: "translate(0, 0)", opacity: 0.9, filter: "blur(0)" },
          {
            transform: `translate(${Math.cos(angle) * distance}px, ${Math.sin(angle) * distance - 6}px)`,
            opacity: 0,
            filter: "blur(1.5px)",
          },
        ],
        { duration: 500 + Math.random() * 360, easing: SOFT },
      )
      .finished.then(() => grain.remove(), () => grain.remove());
  }
}
