// The panel's few controls, drawn with the classes tokens.ts styles for every
// amux surface on the web. Hand-made rather than a component library: there
// are six of them, and the overlay, which shares their look, has to stay
// free of any framework.

import type { ButtonHTMLAttributes, ComponentChildren, SVGAttributes, TextareaHTMLAttributes } from "preact";
import { useEffect, useId, useMemo, useRef, useState } from "preact/hooks";

type ButtonProps = ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: "primary" | "secondary" | "ghost" | "danger";
  size?: "md" | "sm" | "icon";
  type?: "button" | "submit";
};

export function Button({ variant = "secondary", size = "md", type = "button", class: extra, ...rest }: ButtonProps) {
  const classes = [
    "btn",
    variant !== "secondary" && `btn-${variant}`,
    size === "sm" && "btn-sm",
    size === "icon" && "btn-icon",
    extra,
  ]
    .filter(Boolean)
    .join(" ");
  return <button type={type} class={classes} {...rest} />;
}

export function Kbd({ children }: { children: ComponentChildren }) {
  return <kbd class="kbd">{children}</kbd>;
}

/** A 16-point line icon. Decorative: the words beside it say what it means. */
function Svg({ children, ...rest }: { children: ComponentChildren } & SVGAttributes<SVGSVGElement>) {
  return (
    <svg
      aria-hidden="true"
      width={16}
      height={16}
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      stroke-width={1.5}
      {...rest}
    >
      {children}
    </svg>
  );
}

export const Icons = {
  check: () => (
    <Svg stroke-linecap="round" stroke-linejoin="round">
      <path d="M3.5 8.5l3 3 6-7" />
    </Svg>
  ),
  alert: () => (
    <Svg stroke-linecap="round">
      <circle cx="8" cy="8" r="6.25" />
      <path d="M8 4.75v3.75M8 11.1v.15" />
    </Svg>
  ),
  info: () => (
    <Svg stroke-linecap="round">
      <circle cx="8" cy="8" r="6.25" />
      <path d="M8 7.25v4M8 4.9v.15" />
    </Svg>
  ),
  chevrons: () => (
    <Svg stroke-linecap="round" stroke-linejoin="round">
      <path d="M5 6.25L8 3.5l3 2.75M5 9.75L8 12.5l3-2.75" />
    </Svg>
  ),
  close: () => (
    <Svg stroke-linecap="round">
      <path d="M4.5 4.5l7 7M11.5 4.5l-7 7" />
    </Svg>
  ),
  plus: () => (
    <Svg stroke-linecap="round">
      <path d="M8 3.5v9M3.5 8h9" />
    </Svg>
  ),
  /** A block with a pointer on it. */
  block: () => (
    <Svg>
      <rect x="1.75" y="3.25" width="12.5" height="7.5" rx="1.5" />
      <path d="M9 8.5l4 5.5-1.6.2-.9 1.5z" fill="currentColor" stroke="none" />
    </Svg>
  ),
  /** Lines of text with one picked out. */
  text: () => (
    <Svg stroke-linecap="round">
      <path d="M2 4h12M2 12h7" />
      <rect x="1.5" y="6.25" width="9" height="3.5" rx="1" fill="currentColor" stroke="none" opacity="0.35" />
      <path d="M3 8h6" />
    </Svg>
  ),
  /** A dashed region. */
  box: () => (
    <Svg stroke-dasharray="2.5 2">
      <rect x="2" y="2.5" width="12" height="11" rx="1.5" />
    </Svg>
  ),
};

/** A message across the panel: a problem to fix, or how the last send went. */
export function Banner({ tone, children }: { tone: "notice" | "success" | "error"; children: ComponentChildren }) {
  const Icon = tone === "success" ? Icons.check : tone === "error" ? Icons.alert : Icons.info;
  return (
    <div class={`banner banner-${tone}`} role={tone === "error" ? "alert" : "status"}>
      <span class="banner-icon">
        <Icon />
      </span>
      <div class="banner-body">{children}</div>
    </div>
  );
}

/** A text box that grows with what is written, up to a limit. */
export function Textarea(props: TextareaHTMLAttributes<HTMLTextAreaElement> & { value: string }) {
  const ref = useRef<HTMLTextAreaElement>(null);
  useEffect(() => {
    const area = ref.current;
    if (!area) return;
    area.style.height = "auto";
    area.style.height = `${Math.min(area.scrollHeight + 2, 200)}px`;
  }, [props.value]);
  return <textarea ref={ref} class="field" {...props} />;
}

export interface Choice {
  value: string;
  label: string;
  /** A second, quieter line. */
  detail?: string;
  /** A short mark after the label, such as "Needs you". */
  badge?: string;
  icon?: "plus";
}

export interface ChoiceGroup {
  label: string;
  choices: Choice[];
}

interface PickerProps {
  groups: ChoiceGroup[];
  value: string | null;
  placeholder: string;
  onChange(value: string): void;
  /** What the search field says when it is empty. */
  search: string;
}

/**
 * A button that opens a searchable list, grouped, with two lines a row: what
 * a native select cannot show. Keyboard: arrows move, Enter picks, Escape
 * closes; typing filters.
 */
export function Picker({ groups, value, placeholder, onChange, search }: PickerProps) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const [above, setAbove] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const input = useRef<HTMLInputElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const id = useId();

  const all = useMemo(() => groups.flatMap((group) => group.choices), [groups]);
  const chosen = all.find((choice) => choice.value === value) ?? null;
  const shown = useMemo(() => {
    const words = query.trim().toLowerCase();
    if (!words) return groups;
    return groups
      .map((group) => ({
        ...group,
        choices: group.choices.filter((choice) =>
          `${choice.label} ${choice.detail ?? ""} ${group.label}`.toLowerCase().includes(words),
        ),
      }))
      .filter((group) => group.choices.length > 0);
  }, [groups, query]);
  const flat = shown.flatMap((group) => group.choices);

  function show() {
    const box = trigger.current?.getBoundingClientRect();
    setAbove(!!box && box.top > window.innerHeight - box.bottom);
    setQuery("");
    setActive(
      Math.max(
        0,
        all.findIndex((choice) => choice.value === value),
      ),
    );
    setOpen(true);
  }

  function close(refocus = true) {
    setOpen(false);
    if (refocus) trigger.current?.focus();
  }

  function pick(choice: Choice) {
    onChange(choice.value);
    close();
  }

  useEffect(() => {
    if (!open) return;
    input.current?.focus();
    const outside = (event: PointerEvent) => {
      if (!root.current?.contains(event.target as Node)) close(false);
    };
    window.addEventListener("pointerdown", outside, true);
    return () => window.removeEventListener("pointerdown", outside, true);
  }, [open]);

  useEffect(() => {
    list.current?.querySelector(`[data-index="${active}"]`)?.scrollIntoView({ block: "nearest" });
  }, [active, open]);

  function onKey(event: KeyboardEvent) {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setActive((at) => Math.min(flat.length - 1, at + 1));
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      setActive((at) => Math.max(0, at - 1));
    } else if (event.key === "Enter") {
      event.preventDefault();
      const choice = flat[active];
      if (choice) pick(choice);
    } else if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      close();
    } else if (event.key === "Tab") {
      close(false);
    }
  }

  let index = -1;
  return (
    <div class="picker" ref={root}>
      <button
        ref={trigger}
        type="button"
        class="field picker-trigger"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={`${id}-list`}
        onClick={() => (open ? close() : show())}
        onKeyDown={(event) => {
          if (!open && (event.key === "ArrowDown" || event.key === "ArrowUp")) {
            event.preventDefault();
            show();
          }
        }}
      >
        {chosen ? (
          <span class="picker-value">
            <span class="picker-label">{chosen.label}</span>
            {chosen.detail && <span class="picker-detail">{chosen.detail}</span>}
          </span>
        ) : (
          <span class="picker-value picker-placeholder">{placeholder}</span>
        )}
        <span class="picker-chevron">
          <Icons.chevrons />
        </span>
      </button>
      {open && (
        <div class={`picker-pop ${above ? "above" : "below"}`}>
          <input
            ref={input}
            class="picker-search"
            role="combobox"
            aria-expanded="true"
            aria-controls={`${id}-list`}
            aria-activedescendant={flat[active] ? `${id}-${active}` : undefined}
            placeholder={search}
            value={query}
            onInput={(event) => {
              setQuery((event.target as HTMLInputElement).value);
              setActive(0);
            }}
            onKeyDown={onKey}
          />
          <div class="picker-list" role="listbox" id={`${id}-list`} ref={list}>
            {flat.length === 0 && <div class="picker-empty">Nothing matches</div>}
            {shown.map((group) => (
              // biome-ignore lint/a11y/useSemanticElements: an ARIA group of options inside a listbox, not a form's fieldset
              <div key={group.label} role="group" aria-label={group.label}>
                <div class="picker-group" aria-hidden="true">
                  {group.label}
                </div>
                {group.choices.map((choice) => {
                  index += 1;
                  const at = index;
                  return (
                    // biome-ignore lint/a11y/useFocusableInteractive: focus stays in the search field, which names the active option (aria-activedescendant)
                    // biome-ignore lint/a11y/useKeyWithClickEvents: the search field's keys move between and pick options
                    <div
                      key={choice.value}
                      id={`${id}-${at}`}
                      data-index={at}
                      role="option"
                      aria-selected={choice.value === value}
                      class={`picker-option${at === active ? " active" : ""}`}
                      onPointerMove={() => setActive(at)}
                      onClick={() => pick(choice)}
                    >
                      {choice.icon === "plus" && (
                        <span class="picker-option-icon">
                          <Icons.plus />
                        </span>
                      )}
                      <span class="picker-value">
                        <span class="picker-label">
                          {choice.label}
                          {choice.badge && <span class="badge">{choice.badge}</span>}
                        </span>
                        {choice.detail && <span class="picker-detail">{choice.detail}</span>}
                      </span>
                      {choice.value === value && (
                        <span class="picker-check">
                          <Icons.check />
                        </span>
                      )}
                    </div>
                  );
                })}
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
