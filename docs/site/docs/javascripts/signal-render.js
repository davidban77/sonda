/* Sonda docs — rendering shared between the playground and the live widgets.
 *
 * Extracted rather than copied (the #543 precedent). Everything here already
 * existed twice or was about to: `palette`, `formatNumber` and `formatSeconds`
 * were duplicated character-for-character between playground.js and
 * livegen.js, and WP14 needed the histogram, summary and log renderers on
 * generators.md as well as on the playground. Two identical definitions that
 * nothing holds together are one edit away from disagreeing, and the thing
 * they would disagree about is what a reader sees on two pages that are
 * supposed to be teaching the same signal.
 *
 * No wasm and no scenario knowledge: these functions take an already-sampled
 * entry from `sample_scenario` and put pixels or elements on the page. What to
 * sample, and whether there is anything worth drawing, stays with the caller.
 *
 * NOT bundle-coupled. Only sonda-pure.js is compiled into the editor bundle,
 * so changes here do not move the drift gate.
 */

/* Chart colors for the current Material color scheme.
 *
 * The union of what both callers had: playground.js never read `line` and
 * livegen.js never read `plate`, and the six keys they shared were identical
 * strings. Keeping the union means neither caller changed behaviour when its
 * own copy was deleted.
 */
export function palette() {
  const dark = document.body.getAttribute("data-md-color-scheme") === "slate";
  return {
    grid: dark ? "rgba(148, 163, 184, 0.25)" : "rgba(100, 116, 139, 0.25)",
    text: dark ? "#94a3b8" : "#64748b",
    line: "#f97316",
    // Same two washes on both pages, for the same reason: a reader who learns
    // what the grey band means on one page should not have to learn it again
    // on the other.
    gap: dark ? "rgba(148, 163, 184, 0.14)" : "rgba(100, 116, 139, 0.12)",
    burst: dark ? "rgba(253, 186, 116, 0.14)" : "rgba(249, 115, 22, 0.10)",
    // Backing plate for the burst label, which is drawn INSIDE the plot
    // and would otherwise be read through whatever trace passes behind it.
    plate: dark ? "rgba(15, 23, 42, 0.82)" : "rgba(255, 255, 255, 0.82)",
  };
}

export function formatNumber(value) {
  if (Math.abs(value) >= 1000) return value.toFixed(0);
  if (Math.abs(value) >= 10) return value.toFixed(1);
  return value.toFixed(2);
}

export function formatSeconds(secs) {
  const rounded = Math.round(secs);
  if (rounded < 60) return `${rounded}s`;
  const mins = Math.floor(rounded / 60);
  const rest = rounded % 60;
  return rest ? `${mins}m${rest}s` : `${mins}m`;
}

/* Bucket bounds go small: a latency histogram's lowest bucket is routinely
 * 0.005, and `formatNumber` floors at two decimals, so it would render that as
 * "0.01" — the same string as the 0.01 bucket above it. Two axis rows reading
 * the same number is worse than one long row, hence the significant-figure
 * escape hatch below 0.01. */
export function formatBound(value) {
  if (value !== 0 && Math.abs(value) < 0.01) return value.toPrecision(1);
  return formatNumber(value);
}

export function drawHistogramHeatmap(canvas, histogram) {
  const colors = palette();
  const dpr = window.devicePixelRatio || 1;
  const cssWidth = canvas.parentElement.clientWidth;
  const rows = histogram.bucket_bounds.length + 1; // +Inf row on top
  const rowHeight = Math.max(12, Math.min(20, Math.floor(240 / rows)));
  const pad = { left: 64, right: 12, top: 8, bottom: 26 };
  const cssHeight = pad.top + rows * rowHeight + pad.bottom;
  canvas.width = cssWidth * dpr;
  canvas.height = cssHeight * dpr;
  canvas.style.width = cssWidth + "px";
  canvas.style.height = cssHeight + "px";
  const ctx = canvas.getContext("2d");
  ctx.scale(dpr, dpr);
  ctx.clearRect(0, 0, cssWidth, cssHeight);

  const ticks = histogram.counts.length;
  if (!ticks) return;
  const offset = histogram.offset_secs || 0;
  const spanSecs = offset + ticks * histogram.tick_secs;
  const plotW = cssWidth - pad.left - pad.right;
  const x = (secs) => pad.left + (secs / spanSecs) * plotW;
  // Row 0 (lowest bucket) sits at the bottom, like a latency axis.
  const rowY = (row) => pad.top + (rows - 1 - row) * rowHeight;

  let maxCount = 1;
  for (const row of histogram.counts) {
    for (const count of row) if (count > maxCount) maxCount = count;
  }

  const cellW = Math.max(1, (histogram.tick_secs / spanSecs) * plotW);
  histogram.counts.forEach((rowCounts, tick) => {
    const px = x(offset + tick * histogram.tick_secs);
    rowCounts.forEach((count, row) => {
      if (!count) return;
      const alpha = 0.12 + 0.88 * (count / maxCount);
      ctx.fillStyle = `rgba(249, 115, 22, ${alpha.toFixed(3)})`;
      ctx.fillRect(px, rowY(row), cellW + 0.5, rowHeight - 1);
    });
  });

  ctx.fillStyle = colors.text;
  ctx.font = "10px ui-monospace, monospace";
  ctx.textAlign = "right";
  const labelEvery = Math.ceil(rows / 12);
  for (let row = 0; row < rows; row++) {
    if (row % labelEvery !== 0 && row !== rows - 1) continue;
    const label = row === rows - 1 ? "+Inf" : `≤${formatBound(histogram.bucket_bounds[row])}`;
    ctx.fillText(label, pad.left - 6, rowY(row) + rowHeight / 2 + 3);
  }
  ctx.textAlign = "center";
  const xSteps = Math.min(6, Math.max(2, Math.floor(plotW / 110)));
  for (let step = 0; step <= xSteps; step++) {
    const secs = (spanSecs * step) / xSteps;
    ctx.fillText(formatSeconds(secs), x(secs), cssHeight - 8);
  }
}

export function drawSummaryBands(canvas, summary) {
  const colors = palette();
  const dpr = window.devicePixelRatio || 1;
  const cssWidth = canvas.parentElement.clientWidth;
  const cssHeight = 220;
  canvas.width = cssWidth * dpr;
  canvas.height = cssHeight * dpr;
  canvas.style.width = cssWidth + "px";
  canvas.style.height = cssHeight + "px";
  const ctx = canvas.getContext("2d");
  ctx.scale(dpr, dpr);
  ctx.clearRect(0, 0, cssWidth, cssHeight);

  const ticks = summary.values.length;
  const quantileCount = summary.quantiles.length;
  if (!ticks || !quantileCount) return;

  const pad = { left: 48, right: 46, top: 12, bottom: 26 };
  const plotW = cssWidth - pad.left - pad.right;
  const plotH = cssHeight - pad.top - pad.bottom;
  const offset = summary.offset_secs || 0;
  const spanSecs = offset + (ticks - 1) * summary.tick_secs;

  let min = Infinity;
  let max = -Infinity;
  for (const row of summary.values) {
    for (const value of row) {
      if (value < min) min = value;
      if (value > max) max = value;
    }
  }
  if (!Number.isFinite(min)) return;
  if (max - min < 1e-12) {
    min -= 1;
    max += 1;
  }
  const range = max - min;
  min -= range * 0.08;
  max += range * 0.08;
  const x = (tick) => pad.left + ((offset + tick * summary.tick_secs) / spanSecs) * plotW;
  const y = (value) => pad.top + (1 - (value - min) / (max - min)) * plotH;

  ctx.strokeStyle = colors.grid;
  ctx.fillStyle = colors.text;
  ctx.lineWidth = 1;
  ctx.font = "10px ui-monospace, monospace";
  ctx.setLineDash([2, 5]);
  for (let row = 0; row <= 3; row++) {
    const value = min + ((max - min) * row) / 3;
    const gy = y(value);
    ctx.beginPath();
    ctx.moveTo(pad.left, gy);
    ctx.lineTo(cssWidth - pad.right, gy);
    ctx.stroke();
    ctx.textAlign = "right";
    ctx.fillText(formatNumber(value), pad.left - 6, gy + 3);
  }
  ctx.setLineDash([]);

  // Envelope between the lowest and highest quantile, then one line per
  // quantile, brightest at the median end.
  ctx.fillStyle = "rgba(59, 130, 246, 0.14)";
  ctx.beginPath();
  summary.values.forEach((row, tick) => {
    const py = y(row[0]);
    if (tick === 0) ctx.moveTo(x(tick), py);
    else ctx.lineTo(x(tick), py);
  });
  for (let tick = ticks - 1; tick >= 0; tick--) {
    ctx.lineTo(x(tick), y(summary.values[tick][quantileCount - 1]));
  }
  ctx.closePath();
  ctx.fill();

  for (let q = 0; q < quantileCount; q++) {
    const alpha = 0.35 + 0.65 * (1 - q / Math.max(1, quantileCount - 1));
    ctx.strokeStyle = `rgba(59, 130, 246, ${alpha.toFixed(3)})`;
    ctx.lineWidth = q === 0 ? 2 : 1.4;
    ctx.beginPath();
    summary.values.forEach((row, tick) => {
      const py = y(row[q]);
      if (tick === 0) ctx.moveTo(x(tick), py);
      else ctx.lineTo(x(tick), py);
    });
    ctx.stroke();
    ctx.fillStyle = colors.text;
    ctx.textAlign = "left";
    const lastY = y(summary.values[ticks - 1][q]);
    ctx.fillText(`p${Math.round(summary.quantiles[q] * 100)}`, cssWidth - pad.right + 4, lastY + 3);
  }

  ctx.fillStyle = colors.text;
  ctx.textAlign = "center";
  const xSteps = Math.min(6, Math.max(2, Math.floor(plotW / 110)));
  for (let step = 0; step <= xSteps; step++) {
    const secs = (spanSecs * step) / xSteps;
    const px = pad.left + (secs / spanSecs) * plotW;
    ctx.fillText(formatSeconds(secs), px, cssHeight - 8);
  }
}

/* One scrollable block of log lines, each stamped with its offset on the
 * scenario timeline and colored by severity.
 *
 * `prefix` names the BEM block the CSS hangs off, because the two callers are
 * on different pages with different surrounding type. It is a parameter rather
 * than a hard-coded string so the playground keeps the exact class names its
 * stylesheet and its smoke assertions already use — an extraction that renames
 * things is a rewrite wearing an extraction's clothes.
 *
 * Text goes in via `textContent` and `createTextNode`, never markup: a log
 * message is generated content and `check_no_raw_html.sh` holds the floor.
 */
export function logStream(log, { prefix }) {
  const stream = document.createElement("div");
  stream.className = `${prefix}__logstream`;
  for (const line of log.lines) {
    const row = document.createElement("div");
    row.className = `${prefix}__logline ${prefix}__logline--${line.severity}`;
    const at = document.createElement("span");
    at.className = `${prefix}__logat`;
    at.textContent = `+${line.secs.toFixed(line.secs % 1 ? 2 : 0)}s`;
    const sev = document.createElement("span");
    sev.className = `${prefix}__logsev`;
    sev.textContent = line.severity.toUpperCase().padEnd(5);
    row.append(at, sev, document.createTextNode(line.message));
    stream.appendChild(row);
  }
  return stream;
}
