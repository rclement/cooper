// Renders a render_chart tool call as an inline SVG chart (bar/line/scatter)
// plus its accessible twin: a plain HTML table of the same rows, one click
// away via a toggle. Hand-rolled rather than a charting library — this app's
// whole ethos is "no black magic, see how it works" (see the project's
// CLAUDE.md), and the mark specs below (rounded bar ends, surface gaps/rings,
// direct labels that measure before placing, a crosshair tooltip) come
// straight from the dataviz skill rather than a library's defaults.
//
// Colors: the fixed-order categorical palette from chart-common.js, already
// validated (CVD separation, lightness/chroma bands, contrast) against this
// app's actual dark chart surface. Chrome (ink/gridlines/baseline) reuses the
// app's own design tokens (--text, --text-dim, --border) rather than a
// second parallel gray scale.
import { CATEGORICAL_COLORS } from "./chart-common.js";

const NS = "http://www.w3.org/2000/svg";
const VIEW_W = 640;
const MARGIN = { top: 10, right: 16, bottom: 34, left: 48 };

function svgEl(name, attrs = {}) {
  const el = document.createElementNS(NS, name);
  for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, v);
  return el;
}

function textEl(x, y, content, attrs = {}) {
  const el = svgEl("text", { x, y, ...attrs });
  el.textContent = content; // untrusted data (field/category names) — never innerHTML
  return el;
}

// "Nice numbers" axis ticks (Sparks' algorithm): picks a round step so ticks
// land on 0 / 1,000 / 2,000-style values instead of the data's raw extent.
function niceTicks(min, max, targetCount = 5) {
  if (min === max) {
    min -= 1;
    max += 1;
  }
  const range = niceNumber(max - min, false);
  const step = niceNumber(range / (targetCount - 1), true);
  const niceMin = Math.floor(min / step) * step;
  const niceMax = Math.ceil(max / step) * step;
  const ticks = [];
  for (let v = niceMin; v <= niceMax + step / 2; v += step) ticks.push(Math.round(v * 1e9) / 1e9);
  return { min: niceMin, max: niceMax, ticks };
}

function niceNumber(range, round) {
  const exponent = Math.floor(Math.log10(range));
  const fraction = range / 10 ** exponent;
  let niceFraction;
  if (round) {
    if (fraction < 1.5) niceFraction = 1;
    else if (fraction < 3) niceFraction = 2;
    else if (fraction < 7) niceFraction = 5;
    else niceFraction = 10;
  } else {
    if (fraction <= 1) niceFraction = 1;
    else if (fraction <= 2) niceFraction = 2;
    else if (fraction <= 5) niceFraction = 5;
    else niceFraction = 10;
  }
  return niceFraction * 10 ** exponent;
}

function formatTick(v) {
  return Math.abs(v) >= 1000 ? v.toLocaleString() : String(Math.round(v * 100) / 100);
}

function isNumeric(v) {
  return typeof v === "number" && Number.isFinite(v);
}

/// Renders into `container` (a block's body element). `rows`/`spec` are
/// already-validated (see chart-common.js's `parseChartCall`).
export function renderChart(container, rows, spec) {
  container.innerHTML = "";
  const root = document.createElement("div");
  root.className = "chart-viz";
  container.appendChild(root);

  if (spec.title) {
    const title = document.createElement("div");
    title.className = "chart-title";
    title.textContent = spec.title;
    root.appendChild(title);
  }

  const seriesColors = spec.y.map((_, i) => CATEGORICAL_COLORS[i % CATEGORICAL_COLORS.length]);

  if (spec.y.length >= 2) {
    root.appendChild(buildLegend(spec, seriesColors));
  }

  const chartHost = document.createElement("div");
  chartHost.className = "chart-svg-host";
  root.appendChild(chartHost);

  const tableHost = document.createElement("div");
  tableHost.className = "chart-table-host";
  tableHost.hidden = true;
  root.appendChild(tableHost);
  buildTable(tableHost, rows, spec);

  const toggle = document.createElement("button");
  toggle.type = "button";
  toggle.className = "chart-toggle secondary";
  toggle.textContent = "Table view";
  toggle.addEventListener("click", () => {
    const showingTable = !tableHost.hidden;
    tableHost.hidden = showingTable;
    chartHost.hidden = !showingTable;
    toggle.textContent = showingTable ? "Table view" : "Chart view";
  });
  root.appendChild(toggle);

  const tooltip = document.createElement("div");
  tooltip.className = "chart-tooltip";
  tooltip.hidden = true;
  root.appendChild(tooltip);

  if (spec.type === "bar") {
    renderBar(chartHost, tooltip, root, rows, spec, seriesColors);
  } else if (spec.type === "line") {
    renderLine(chartHost, tooltip, root, rows, spec, seriesColors);
  } else {
    renderScatter(chartHost, tooltip, root, rows, spec, seriesColors);
  }
}

function buildLegend(spec, colors) {
  const legend = document.createElement("div");
  legend.className = "chart-legend";
  spec.y.forEach((field, i) => {
    const item = document.createElement("span");
    item.className = "chart-legend-item";
    const swatch = document.createElement("span");
    swatch.className = "chart-legend-swatch";
    swatch.style.background = colors[i];
    const label = document.createElement("span");
    label.textContent = field;
    item.append(swatch, label);
    legend.appendChild(item);
  });
  return legend;
}

function buildTable(host, rows, spec) {
  const table = document.createElement("table");
  table.className = "chart-table";
  const fields = [spec.x, ...spec.y];

  const thead = document.createElement("thead");
  const headRow = document.createElement("tr");
  for (const field of fields) {
    const th = document.createElement("th");
    th.textContent = field;
    headRow.appendChild(th);
  }
  thead.appendChild(headRow);
  table.appendChild(thead);

  const tbody = document.createElement("tbody");
  for (const row of rows) {
    const tr = document.createElement("tr");
    for (const field of fields) {
      const td = document.createElement("td");
      td.textContent = String(row[field] ?? "");
      tr.appendChild(td);
    }
    tbody.appendChild(tr);
  }
  table.appendChild(tbody);
  host.appendChild(table);
}

function positionTooltip(root, tooltip, clientX, clientY) {
  const rootRect = root.getBoundingClientRect();
  tooltip.style.left = `${clientX - rootRect.left + 12}px`;
  tooltip.style.top = `${clientY - rootRect.top + 12}px`;
}

// Values lead (Strong/high-contrast), series name follows (secondary) — the
// legend's hierarchy inverted, per the interaction spec.
function fillTooltip(tooltip, categoryLabel, entries) {
  tooltip.innerHTML = "";
  const head = document.createElement("div");
  head.className = "chart-tooltip-head";
  head.textContent = categoryLabel;
  tooltip.appendChild(head);
  for (const { color, name, value } of entries) {
    const row = document.createElement("div");
    row.className = "chart-tooltip-row";
    const key = document.createElement("span");
    key.className = "chart-tooltip-key";
    key.style.background = color;
    const val = document.createElement("strong");
    val.textContent = value;
    const name_ = document.createElement("span");
    name_.className = "chart-tooltip-name";
    name_.textContent = name;
    row.append(key, val, name_);
    tooltip.appendChild(row);
  }
  tooltip.hidden = false;
}

function plotWidth() {
  return VIEW_W - MARGIN.left - MARGIN.right;
}

function makeSvg(height) {
  return svgEl("svg", {
    viewBox: `0 0 ${VIEW_W} ${height}`,
    width: "100%",
    role: "img",
  });
}

function drawYAxis(svg, plotH, domainMin, domainMax, ticks) {
  const yFor = (v) => MARGIN.top + plotH - ((v - domainMin) / (domainMax - domainMin)) * plotH;
  for (const tick of ticks) {
    const y = yFor(tick);
    svg.appendChild(
      svgEl("line", {
        x1: MARGIN.left,
        x2: MARGIN.left + plotWidth(),
        y1: y,
        y2: y,
        class: "chart-gridline",
      }),
    );
    svg.appendChild(textEl(MARGIN.left - 8, y + 3, formatTick(tick), { class: "chart-axis-label", "text-anchor": "end" }));
  }
  return yFor;
}

function drawXLabels(svg, plotH, positions, rotate) {
  const y = MARGIN.top + plotH + 16;
  for (const { x, label } of positions) {
    const attrs = { class: "chart-axis-label", "text-anchor": rotate ? "end" : "middle" };
    if (rotate) attrs.transform = `rotate(-30 ${x} ${y})`;
    svg.appendChild(textEl(x, y, label, attrs));
  }
}

function fitsAbove(yTop) {
  return yTop - MARGIN.top >= 14;
}

function renderBar(host, tooltip, root, rows, spec, colors) {
  const categories = rows.map((r) => String(r[spec.x]));
  const values = rows.flatMap((r) => spec.y.map((f) => Number(r[f]) || 0));
  const dataMin = Math.min(0, ...values);
  const dataMax = Math.max(0, ...values);
  const { min: domainMin, max: domainMax, ticks } = niceTicks(dataMin, dataMax);

  const plotH = 220;
  const height = plotH + MARGIN.top + MARGIN.bottom;
  const svg = makeSvg(height);
  host.appendChild(svg);

  const yFor = drawYAxis(svg, plotH, domainMin, domainMax, ticks);
  const zeroY = yFor(0);
  svg.appendChild(
    svgEl("line", { x1: MARGIN.left, x2: MARGIN.left + plotWidth(), y1: zeroY, y2: zeroY, class: "chart-baseline" }),
  );

  const bandWidth = plotWidth() / categories.length;
  const groupWidth = bandWidth * 0.6;
  const seriesCount = spec.y.length;
  const gap = 2;
  const barWidth = Math.min(24, (groupWidth - (seriesCount - 1) * gap) / seriesCount);
  const groupUsedWidth = barWidth * seriesCount + gap * (seriesCount - 1);

  const totalMarks = categories.length * seriesCount;
  const labelBudgetOk = totalMarks <= 12;
  const xLabelPositions = [];

  categories.forEach((cat, ci) => {
    const groupCenter = MARGIN.left + bandWidth * ci + bandWidth / 2;
    const groupStart = groupCenter - groupUsedWidth / 2;
    xLabelPositions.push({ x: groupCenter, label: cat });

    spec.y.forEach((field, si) => {
      const value = Number(rows[ci][field]) || 0;
      const barX = groupStart + si * (barWidth + gap);
      const y0 = yFor(0);
      const y1 = yFor(value);
      const top = Math.min(y0, y1);
      const barH = Math.abs(y1 - y0);
      const rectEl = svgEl("rect", {
        x: barX,
        y: top,
        width: barWidth,
        height: Math.max(barH, 0.5),
        rx: 4,
        fill: colors[si],
        class: "chart-bar-mark",
        tabindex: "0",
        role: "img",
        "aria-label": `${cat}, ${field}: ${formatTick(value)}`,
      });
      rectEl.addEventListener("pointerenter", (e) => {
        rectEl.classList.add("is-hovered");
        fillTooltip(tooltip, cat, [{ color: colors[si], name: field, value: formatTick(value) }]);
        positionTooltip(root, tooltip, e.clientX, e.clientY);
      });
      rectEl.addEventListener("pointermove", (e) => positionTooltip(root, tooltip, e.clientX, e.clientY));
      rectEl.addEventListener("pointerleave", () => {
        rectEl.classList.remove("is-hovered");
        tooltip.hidden = true;
      });
      rectEl.addEventListener("focus", (e) => {
        const box = rectEl.getBoundingClientRect();
        rectEl.classList.add("is-hovered");
        fillTooltip(tooltip, cat, [{ color: colors[si], name: field, value: formatTick(value) }]);
        positionTooltip(root, tooltip, box.left, box.top);
      });
      rectEl.addEventListener("blur", () => {
        rectEl.classList.remove("is-hovered");
        tooltip.hidden = true;
      });
      svg.appendChild(rectEl);

      if (labelBudgetOk && fitsAbove(top)) {
        svg.appendChild(
          textEl(barX + barWidth / 2, top - 5, formatTick(value), {
            class: "chart-direct-label",
            "text-anchor": "middle",
          }),
        );
      }
    });
  });

  drawXLabels(svg, plotH, xLabelPositions, categories.length > 8);
}

function renderLine(host, tooltip, root, rows, spec, colors) {
  const xNumeric = rows.every((r) => isNumeric(r[spec.x]));
  const values = rows.flatMap((r) => spec.y.map((f) => Number(r[f]) || 0));
  const { min: domainMin, max: domainMax, ticks } = niceTicks(Math.min(...values), Math.max(...values));

  const plotH = 220;
  const height = plotH + MARGIN.top + MARGIN.bottom;
  const svg = makeSvg(height);
  host.appendChild(svg);

  const yFor = drawYAxis(svg, plotH, domainMin, domainMax, ticks);

  let xFor;
  const xLabelPositions = [];
  if (xNumeric) {
    const xs = rows.map((r) => Number(r[spec.x]));
    const xMin = Math.min(...xs);
    const xMax = Math.max(...xs);
    xFor = (v) => MARGIN.left + ((v - xMin) / (xMax - xMin || 1)) * plotWidth();
    const step = Math.max(1, Math.ceil(rows.length / 8));
    rows.forEach((r, i) => {
      if (i % step === 0) xLabelPositions.push({ x: xFor(Number(r[spec.x])), label: formatTick(Number(r[spec.x])) });
    });
  } else {
    const bandWidth = plotWidth() / Math.max(1, rows.length - 1 || 1);
    xFor = (i) => MARGIN.left + (rows.length > 1 ? i * bandWidth : plotWidth() / 2);
    const step = Math.max(1, Math.ceil(rows.length / 8));
    rows.forEach((r, i) => {
      if (i % step === 0) xLabelPositions.push({ x: xFor(i), label: String(r[spec.x]) });
    });
  }
  drawXLabels(svg, plotH, xLabelPositions, !xNumeric && rows.length > 8);

  const endYs = [];
  spec.y.forEach((field, si) => {
    const points = rows.map((r, i) => ({
      x: xNumeric ? xFor(Number(r[spec.x])) : xFor(i),
      y: yFor(Number(r[field]) || 0),
    }));
    const d = points.map((p, i) => `${i === 0 ? "M" : "L"}${p.x},${p.y}`).join(" ");
    svg.appendChild(svgEl("path", { d, class: "chart-line-mark", stroke: colors[si], fill: "none" }));

    const last = points[points.length - 1];
    svg.appendChild(svgEl("circle", { cx: last.x, cy: last.y, r: 6, class: "chart-marker-ring" }));
    svg.appendChild(svgEl("circle", { cx: last.x, cy: last.y, r: 4, fill: colors[si] }));
    endYs.push({ y: last.y, field, x: last.x, color: colors[si] });
  });

  // Collision check: end-labels only if consecutive series' end-points clear
  // a minimum vertical gap once sorted — converging lines fall back to the
  // legend + tooltip instead of stacking illegible labels (see marks spec).
  const sorted = [...endYs].sort((a, b) => a.y - b.y);
  const canLabelEnds =
    spec.y.length <= 4 && sorted.every((e, i) => i === 0 || e.y - sorted[i - 1].y >= 14);
  if (canLabelEnds) {
    for (const e of endYs) {
      svg.appendChild(textEl(e.x + 8, e.y + 3, e.field, { class: "chart-direct-label", "text-anchor": "start" }));
    }
  }

  // Crosshair + shared tooltip: one invisible full-height hit rect finds the
  // nearest X and reports every series at that X, keyboard-reachable too.
  const crosshair = svgEl("line", {
    x1: 0, x2: 0, y1: MARGIN.top, y2: MARGIN.top + plotH, class: "chart-crosshair", visibility: "hidden",
  });
  svg.appendChild(crosshair);

  const showAt = (index, clientX, clientY) => {
    const row = rows[index];
    const x = xNumeric ? xFor(Number(row[spec.x])) : xFor(index);
    crosshair.setAttribute("x1", x);
    crosshair.setAttribute("x2", x);
    crosshair.setAttribute("visibility", "visible");
    const entries = spec.y.map((field, si) => ({
      color: colors[si],
      name: field,
      value: formatTick(Number(row[field]) || 0),
    }));
    fillTooltip(tooltip, String(row[spec.x]), entries);
    positionTooltip(root, tooltip, clientX, clientY);
  };

  const hitRect = svgEl("rect", {
    x: MARGIN.left, y: MARGIN.top, width: plotWidth(), height: plotH,
    fill: "transparent", tabindex: "0", class: "chart-hit-layer",
  });
  let focusIndex = 0;
  hitRect.addEventListener("pointermove", (e) => {
    const box = svg.getBoundingClientRect();
    const relX = ((e.clientX - box.left) / box.width) * VIEW_W;
    const nearest = rows.reduce((best, _, i) => {
      const x = xNumeric ? xFor(Number(rows[i][spec.x])) : xFor(i);
      return Math.abs(x - relX) < Math.abs((xNumeric ? xFor(Number(rows[best][spec.x])) : xFor(best)) - relX) ? i : best;
    }, 0);
    focusIndex = nearest;
    showAt(nearest, e.clientX, e.clientY);
  });
  hitRect.addEventListener("pointerleave", () => {
    crosshair.setAttribute("visibility", "hidden");
    tooltip.hidden = true;
  });
  hitRect.addEventListener("keydown", (e) => {
    if (e.key === "ArrowRight") focusIndex = Math.min(rows.length - 1, focusIndex + 1);
    else if (e.key === "ArrowLeft") focusIndex = Math.max(0, focusIndex - 1);
    else return;
    e.preventDefault();
    const box = svg.getBoundingClientRect();
    const x = xNumeric ? xFor(Number(rows[focusIndex][spec.x])) : xFor(focusIndex);
    showAt(focusIndex, box.left + (x / VIEW_W) * box.width, box.top);
  });
  hitRect.addEventListener("focus", () => showAt(focusIndex, ...(() => {
    const box = svg.getBoundingClientRect();
    const x = xNumeric ? xFor(Number(rows[focusIndex][spec.x])) : xFor(focusIndex);
    return [box.left + (x / VIEW_W) * box.width, box.top];
  })()));
  hitRect.addEventListener("blur", () => {
    crosshair.setAttribute("visibility", "hidden");
    tooltip.hidden = true;
  });
  svg.appendChild(hitRect);
}

function renderScatter(host, tooltip, root, rows, spec, colors) {
  const field = spec.y[0];
  if (spec.y.length > 1) {
    // Scatter is inherently one relationship (x vs y); extra y fields would
    // need a series/color-by field instead of a second measure — keep the
    // single-axis rule and just plot the first.
  }
  const xs = rows.map((r) => Number(r[spec.x]));
  const ys = rows.map((r) => Number(r[field]));
  if (!rows.every((r) => isNumeric(r[spec.x]))) {
    host.appendChild(Object.assign(document.createElement("p"), {
      className: "hint",
      textContent: "Scatter charts need a numeric x field.",
    }));
    return;
  }

  const xTicks = niceTicks(Math.min(...xs), Math.max(...xs));
  const yTicks = niceTicks(Math.min(...ys), Math.max(...ys));

  const plotH = 240;
  const height = plotH + MARGIN.top + MARGIN.bottom;
  const svg = makeSvg(height);
  host.appendChild(svg);

  const yFor = drawYAxis(svg, plotH, yTicks.min, yTicks.max, yTicks.ticks);
  const xFor = (v) => MARGIN.left + ((v - xTicks.min) / (xTicks.max - xTicks.min || 1)) * plotWidth();
  drawXLabels(svg, plotH, xTicks.ticks.map((t) => ({ x: xFor(t), label: formatTick(t) })), false);

  rows.forEach((row, i) => {
    const cx = xFor(xs[i]);
    const cy = yFor(ys[i]);
    const hit = svgEl("circle", {
      cx, cy, r: 12, fill: "transparent", tabindex: "0", class: "chart-hit-layer",
      "aria-label": `${spec.x}: ${formatTick(xs[i])}, ${field}: ${formatTick(ys[i])}`,
    });
    const dot = svgEl("circle", { cx, cy, r: 4, fill: colors[0], class: "chart-scatter-mark" });
    const showTip = (clientX, clientY) => {
      fillTooltip(tooltip, `${spec.x}: ${formatTick(xs[i])}`, [
        { color: colors[0], name: field, value: formatTick(ys[i]) },
      ]);
      positionTooltip(root, tooltip, clientX, clientY);
      dot.classList.add("is-hovered");
    };
    hit.addEventListener("pointerenter", (e) => showTip(e.clientX, e.clientY));
    hit.addEventListener("pointermove", (e) => positionTooltip(root, tooltip, e.clientX, e.clientY));
    hit.addEventListener("pointerleave", () => {
      dot.classList.remove("is-hovered");
      tooltip.hidden = true;
    });
    hit.addEventListener("focus", () => {
      const box = hit.getBoundingClientRect();
      showTip(box.left, box.top);
    });
    hit.addEventListener("blur", () => {
      dot.classList.remove("is-hovered");
      tooltip.hidden = true;
    });
    svg.append(dot, hit);
  });
}
