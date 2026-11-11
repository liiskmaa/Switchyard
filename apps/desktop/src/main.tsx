import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "./App";
import { startCoreSync } from "./stores/core";
import "./styles.css";

const root = document.getElementById("root");
if (!root) throw new Error("#root element missing from index.html");

startCoreSync().catch((error: unknown) => {
  console.error("Failed to start Core synchronisation", error);
});

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
