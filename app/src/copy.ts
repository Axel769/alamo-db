// "Copy" buttons: the text goes to the clipboard and the button says
// "Copied" for a moment. `resetOn` changed (another cell, another value)
// = the button goes back to "Copy" at once.
import { useEffect, useRef, useState } from "react";

export function useCopy(resetOn?: unknown): [copied: boolean, copy: (text: string) => void] {
  const [copied, setCopied] = useState(false);
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => {
    window.clearTimeout(timer.current);
    setCopied(false);
  }, [resetOn]);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  const copy = (text: string) =>
    void navigator.clipboard.writeText(text).then(() => {
      setCopied(true);
      window.clearTimeout(timer.current);
      timer.current = window.setTimeout(() => setCopied(false), 1500);
    });
  return [copied, copy];
}
