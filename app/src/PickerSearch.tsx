// Search field at the top of the popovers of the table tab bar
// ("+ Filter", "Sort"): with some text a × appears that clears it, and Esc
// does the same; with an empty field Esc goes to `onKeyDown` (closes the popover).
import { InputHTMLAttributes, KeyboardEvent, useRef } from "react";
import { t } from "./i18n";
import { IconX } from "./icons";

export default function PickerSearch({
  value,
  onChange,
  onKeyDown,
  ...rest
}: {
  value: string;
  onChange: (value: string) => void;
  onKeyDown?: (e: KeyboardEvent<HTMLInputElement>) => void;
} & Omit<InputHTMLAttributes<HTMLInputElement>, "value" | "onChange" | "onKeyDown">) {
  const input = useRef<HTMLInputElement | null>(null);
  return (
    <div className="picker-search">
      <input
        {...rest}
        ref={input}
        className="db-picker-filter"
        autoFocus
        value={value}
        autoCapitalize="none"
        autoCorrect="off"
        spellCheck={false}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Escape" && value !== "") {
            e.preventDefault();
            e.stopPropagation();
            onChange("");
            return;
          }
          onKeyDown?.(e);
        }}
      />
      {value !== "" && (
        <button
          className="chip-x picker-search-x"
          title={t("Clear the search")}
          aria-label={t("Clear the search")}
          onClick={() => {
            onChange("");
            input.current?.focus();
          }}
        >
          <IconX />
        </button>
      )}
    </div>
  );
}
