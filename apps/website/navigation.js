class ToknNavbar extends HTMLElement {
  cleanup;
  connectedCallback() {
    this.cleanup?.();
    const menu = this.querySelector("[data-nav-menu]");
    const toggle = menu?.querySelector("summary");
    if (!menu || !toggle)
      return;
    const controller = new AbortController;
    const options = { signal: controller.signal };
    const closeMenu = (restore_focus) => {
      menu.open = false;
      if (restore_focus)
        toggle.focus();
    };
    menu.addEventListener("keydown", (event) => {
      if (event.key === "Escape" && menu.open) {
        event.preventDefault();
        closeMenu(true);
      }
    }, options);
    menu.addEventListener("click", (event) => {
      if (!(event.target instanceof Element))
        return;
      const link = event.target.closest("a[href]");
      if (!link || event.ctrlKey || event.metaKey || event.shiftKey || event.altKey)
        return;
      const target_url = new URL(link.href, location.href);
      const same_page = target_url.origin === location.origin && target_url.pathname === location.pathname && target_url.search === location.search;
      closeMenu(false);
      if (same_page && target_url.hash) {
        let target = null;
        try {
          target = document.getElementById(decodeURIComponent(target_url.hash.slice(1)));
        } catch {}
        if (target) {
          const temporary_focus = !target.hasAttribute("tabindex");
          if (temporary_focus)
            target.setAttribute("tabindex", "-1");
          target.focus({ preventScroll: true });
          if (temporary_focus)
            target.addEventListener("blur", () => target.removeAttribute("tabindex"), { once: true });
        } else {
          toggle.focus();
        }
      }
    }, options);
    document.addEventListener("pointerdown", (event) => {
      if (menu.open && event.target instanceof Node && !this.contains(event.target))
        closeMenu(menu.contains(document.activeElement));
    }, options);
    const observer = new ResizeObserver(() => {
      const focused = document.activeElement;
      if (getComputedStyle(menu).display !== "none" && focused instanceof HTMLElement && this.contains(focused) && focused.getClientRects().length === 0) {
        toggle.focus();
      }
      if (getComputedStyle(menu).display === "none") {
        if (focused instanceof HTMLAnchorElement && menu.contains(focused)) {
          const equivalent = [...this.querySelectorAll(".tokn-nav-desktop a, .tokn-nav-utilities a")].find((link) => link.href === focused.href);
          (equivalent ?? this.querySelector(".tokn-nav-brand"))?.focus();
        } else if (menu.contains(focused)) {
          this.querySelector(".tokn-nav-brand")?.focus();
        }
        menu.open = false;
      }
    });
    observer.observe(this);
    this.cleanup = () => {
      controller.abort();
      observer.disconnect();
    };
  }
  disconnectedCallback() {
    this.cleanup?.();
  }
}
if (!customElements.get("tokn-navbar"))
  customElements.define("tokn-navbar", ToknNavbar);
