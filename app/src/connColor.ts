// Colour of a connection: the one chosen in the profile or, if missing, an
// automatic, stable one derived from the name. Without it, profiles with no
// colour were all grey: dots, tabs and selection could not be told apart —
// and the colour is precisely what says "which database am I working on".
//
// The automatic palette avoids red (read as production) and grey (read as
// disconnected).
const AUTO = ["#2563eb", "#16a34a", "#d97706", "#7c3aed", "#0d9488", "#c026d3", "#0284c7", "#65a30d"];

export function connColor(p: { color: string | null; name: string } | null | undefined): string {
  if (!p) return "var(--cell-divider)";
  if (p.color) return p.color;
  let h = 0;
  for (let i = 0; i < p.name.length; i++) h = (h * 31 + p.name.charCodeAt(i)) >>> 0;
  return AUTO[h % AUTO.length];
}
