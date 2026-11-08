// Firefox prefs for the Surface RT (Tegra 3, 2 GB RAM, grate = GLES 2.0 -> software WebRender).
// Copied to ~/.config/mozilla/firefox/<profile>/user.js; applied at each Firefox start.
// To undo: delete user.js, then reset the prefs in about:config (user.js values stay in prefs.js).

// RAM / CPU: fewer content processes (default 8)
user_pref("dom.ipc.processCount", 2);
user_pref("dom.ipc.processCount.webIsolated", 1);
user_pref("browser.tabs.unloadOnLowMemory", true);

// Less animation work for the CPU renderer
user_pref("general.smoothScroll", false);
user_pref("toolkit.cosmeticAnimations.enabled", false);
user_pref("ui.prefersReducedMotion", 1);

// Software WebRender explicitly (no futile hardware probing on GLES 2.0)
user_pref("gfx.webrender.software", true);

// Background work: telemetry and studies off
user_pref("datareporting.healthreport.uploadEnabled", false);
user_pref("datareporting.policy.dataSubmissionEnabled", false);
user_pref("toolkit.telemetry.enabled", false);
user_pref("toolkit.telemetry.unified", false);
user_pref("app.shield.optoutstudies.enabled", false);
user_pref("app.normandy.enabled", false);

// New tab page: plain (no sponsored content, top stories, weather)
user_pref("browser.newtabpage.activity-stream.feeds.section.topstories", false);
user_pref("browser.newtabpage.activity-stream.showSponsored", false);
user_pref("browser.newtabpage.activity-stream.showSponsoredTopSites", false);
user_pref("browser.newtabpage.activity-stream.showWeather", false);
user_pref("browser.newtabpage.activity-stream.feeds.topsites", false);
