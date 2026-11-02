import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import BenchApp from "./BenchApp";
import { loadEngines } from "./engines";
import Studio from "./Studio";

// Cold start (§15): first useful frame of the shell, before the data.
let reportedReady = false;

export default function App() {
  const [auto, setAuto] = useState<boolean | null>(null);

  useEffect(() => {
    if (!reportedReady) {
      reportedReady = true;
      requestAnimationFrame(() => {
        void invoke("report_ready");
      });
    }
    Promise.all([invoke<{ auto: boolean }>("harness_mode"), loadEngines()]).then(([m]) => setAuto(m.auto));
  }, []);

  if (auto === null) return null;
  return auto ? <BenchApp /> : <Studio />;
}
