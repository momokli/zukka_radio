// =========================================
// ZUKKAFABRIK MINIMAL ATMOSPHERE JS
// =========================================

document.addEventListener("DOMContentLoaded", function () {
  // --- 1. TELEGRAM/SOCIAL LINK PREVIEW FIX ---
  const ogTitle = "ZUKKAFABRIK KOLLEKTIV RADIO";
  const ogDescription = "Nerdy, low-key underground insider kollektiv radio.";
  // !!! WICHTIG: HIER DEINE QUADRATISCHE BILD-URL EINFÜGEN !!!
  const ogImageURL = "https://DEINE-URL-ZUM-QUADRATISCHEN-BILD.jpg";
  // ----------------------------------------------------------

  function injectMeta(property, content) {
    if (
      !document.querySelector('meta[property="' + property + '"]') &&
      content !== "https://DEINE-URL-ZUM-QUADRATISCHEN-BILD.jpg"
    ) {
      const meta = document.createElement("meta");
      meta.setAttribute("property", property);
      meta.setAttribute("content", content);
      document.head.appendChild(meta);
    }
  }
  injectMeta("og:site_name", ogTitle);
  injectMeta("og:title", ogTitle);
  injectMeta("og:description", ogDescription);
  injectMeta("og:image", ogImageURL);
  injectMeta("og:type", "website");

  // --- 2. ATMOSPHÄRE (Statittisches Rauschen & Flackern) ---
  const bodyElement = document.body;

  // 2a. Statisches Rauschen Overlay (Sehr subtil)
  const noiseOverlay = document.createElement("div");
  // Base64 codiertes Rausch-Muster, transparent
  noiseOverlay.style.cssText =
    "position:fixed;top:0;left:0;width:100%;height:100%;background-image:url('data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAADIAAAAyBAMAAADsEZWCAAAAGFBMVEUAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAMwqO3AAAACHRSTlMABQUHCAkKDHZv8g4AAAAnSURBVDjLY2AgDjAzaCCwM2hAgFlBgwKGBg0KGBo0KGBo0KGBgQEAD24I7eC6V50AAAAASUVORK5CYII=');opacity:0.03;pointer-events:none;z-index:9999;mix-blend-mode:overlay;";
  bodyElement.appendChild(noiseOverlay);

  // 2b. Subtiles Strom-Flackern
  function subtleFlicker() {
    // Nur selten flackern (Wahrscheinlichkeit > 0.97)
    if (Math.random() > 0.97) {
      // Kurz etwas dunkler und gelblicher (Sepia) werden
      bodyElement.style.filter = "brightness(0.94) sepia(10%) contrast(1.05)";
      bodyElement.style.transition = "filter 0.05s ease-out"; // Schneller Dip

      // Zufällige kurze Dauer für das Flackern
      const flickerDuration = Math.floor(Math.random() * 120) + 40;

      setTimeout(() => {
        // Langsamer zurück zum Normalzustand
        bodyElement.style.transition = "filter 0.4s ease-in";
        bodyElement.style.filter = "brightness(1.0) sepia(0%) contrast(1.0)";
      }, flickerDuration);
    }
  }

  // Check alle 700ms
  setInterval(subtleFlicker, 700);
});
