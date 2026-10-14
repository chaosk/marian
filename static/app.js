document.querySelectorAll("[data-confirm]").forEach((form) => {
  form.addEventListener("submit", (event) => {
    if (!window.confirm(form.getAttribute("data-confirm"))) {
      event.preventDefault();
    }
  });
});
const line = document.querySelector("#command-line");
if (line) line.focus();

document.querySelectorAll(".spoiler").forEach((el) => {
  el.addEventListener("click", () => el.classList.toggle("open"));
});

const zoneFilter = document.querySelector("#zone-filter");
const zoneList = document.querySelector("#zone-list");
if (zoneFilter && zoneList) {
  const saved = [...zoneList.querySelectorAll("optgroup")].map((group) => ({
    label: group.label,
    options: [...group.querySelectorAll("option")].map((option) => ({
      value: option.value,
      label: option.textContent,
      selected: option.selected,
    })),
  }));
  const paint = () => {
    const needle = zoneFilter.value.trim().toLowerCase();
    const selected = zoneList.value;
    zoneList.replaceChildren();
    for (const group of saved) {
      const matches = group.options.filter((option) => {
        if (option.value === selected) return true;
        if (!needle) return true;
        return (
          option.label.toLowerCase().includes(needle) ||
          option.value.toLowerCase().includes(needle)
        );
      });
      if (!matches.length) continue;
      const node = document.createElement("optgroup");
      node.label = group.label;
      for (const option of matches) {
        const el = document.createElement("option");
        el.value = option.value;
        el.textContent = option.label;
        if (option.value === selected) el.selected = true;
        node.append(el);
      }
      zoneList.append(node);
    }
  };
  zoneFilter.addEventListener("input", paint);
}
