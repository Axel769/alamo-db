// Production marker (§9.7): icon and word together, never the colour
// alone, so it reads on every surface and for everyone.
import { IconAlert } from "./icons";
import { t } from "./i18n";

export function ProdTag({ long }: { long?: boolean }) {
  return (
    <span className="prod-tag">
      <IconAlert />
      {long ? t("production") : "prod"}
    </span>
  );
}
