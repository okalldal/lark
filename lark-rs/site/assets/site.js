// lark-rs showcase — dependency-free interactivity.
// Everything reads from the single #site-data JSON block, so the figures and
// the engine picker have exactly one source of truth on the page.

(function () {
  "use strict";

  const dataEl = document.getElementById("site-data");
  if (!dataEl) return;

  let data;
  try {
    data = JSON.parse(dataEl.textContent);
  } catch (err) {
    console.error("site-data parse error", err);
    return;
  }

  const STATUS_LABEL = {
    verified: "Verified — enforced by an oracle, bank or gate",
    measured: "Measured — observed in a named benchmark",
    goal: "Goal — a stated direction, not yet shown",
    open: "Open — a known limitation",
  };

  // ---- evidence strip -----------------------------------------------------
  const grid = document.getElementById("evidence-grid");
  if (grid && Array.isArray(data.evidence)) {
    grid.innerHTML = "";
    let allVerified = true;
    data.evidence.forEach((e) => {
      if (e.status !== "verified") allVerified = false;
      const a = document.createElement("a");
      a.className = "ev";
      a.dataset.status = e.status;
      if (e.href) a.href = e.href;
      a.title = STATUS_LABEL[e.status] || e.status;
      const num = document.createElement("span");
      num.className = "num";
      num.textContent = e.value;
      const name = document.createElement("span");
      name.className = "name";
      name.textContent = e.name;
      a.append(num, name);
      grid.appendChild(a);
    });
    const legend = document.getElementById("evidence-legend");
    if (legend) {
      legend.innerHTML = allVerified
        ? 'Every figure above is <strong>Verified</strong> — enforced by an oracle bank or deterministic gate, sourced from the status ledger.'
        : "Figures are sourced from the status ledger.";
    }
  }

  // ---- engine picker ------------------------------------------------------
  const tabs = document.getElementById("engine-tabs");
  const desc = document.getElementById("engine-desc");
  const codeEl = document.getElementById("engine-code");
  const engines = Array.isArray(data.engines) ? data.engines : [];

  function selectEngine(idx) {
    const eng = engines[idx];
    if (!eng) return;

    Array.from(tabs.children).forEach((btn, i) => {
      btn.setAttribute("aria-selected", String(i === idx));
      btn.tabIndex = i === idx ? 0 : -1;
    });

    const facts = eng.facts || {};
    const dl = Object.entries(facts)
      .map(([k, v]) => `<dt>${escapeHtml(k)}</dt><dd>${escapeHtml(v)}</dd>`)
      .join("");
    desc.innerHTML =
      `<h3>${escapeHtml(eng.title)}</h3>` +
      `<p class="use">${escapeHtml(eng.use)}</p>` +
      `<dl>${dl}</dl>`;

    codeEl.textContent = eng.code;
  }

  if (tabs && engines.length) {
    engines.forEach((eng, i) => {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.setAttribute("role", "tab");
      btn.textContent = eng.label;
      btn.addEventListener("click", () => selectEngine(i));
      btn.addEventListener("keydown", (ev) => {
        if (ev.key === "ArrowRight" || ev.key === "ArrowLeft") {
          ev.preventDefault();
          const next =
            (i + (ev.key === "ArrowRight" ? 1 : engines.length - 1)) % engines.length;
          tabs.children[next].focus();
          selectEngine(next);
        }
      });
      tabs.appendChild(btn);
    });
    selectEngine(0);
  }

  // ---- copy buttons -------------------------------------------------------
  document.querySelectorAll(".copy-btn").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const target = document.getElementById(btn.dataset.copy);
      if (!target) return;
      const text = target.textContent;
      try {
        await navigator.clipboard.writeText(text);
        const prev = btn.textContent;
        btn.textContent = "Copied";
        setTimeout(() => (btn.textContent = prev), 1400);
      } catch (err) {
        console.error("copy failed", err);
      }
    });
  });

  function escapeHtml(s) {
    return String(s).replace(/[&<>"']/g, (c) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c])
    );
  }
})();
