document.querySelectorAll("[data-confirm]").forEach((form) => {
  form.addEventListener("submit", (event) => {
    if (!window.confirm(form.getAttribute("data-confirm"))) {
      event.preventDefault();
    }
  });
});
const line = document.querySelector("#command-line");
if (line) line.focus();

const voteToggle = document.querySelector('input[name="vote"]');
const startInput = document.querySelector("#start-input");
const startLabel = document.querySelector("#start-label");
if (voteToggle && startInput && startLabel) {
  const applyStart = () => {
    if (voteToggle.checked) {
      const raw = startInput.value;
      const time = raw.includes("T") ? raw.split("T")[1].slice(0, 5) : raw.slice(0, 5);
      startInput.type = "time";
      startInput.value = time;
      startLabel.textContent = "Clock time";
    } else {
      const raw = startInput.value;
      startInput.type = "datetime-local";
      if (raw && !raw.includes("T")) {
        const today = new Date();
        const month = String(today.getMonth() + 1).padStart(2, "0");
        const day = String(today.getDate()).padStart(2, "0");
        startInput.value = `${today.getFullYear()}-${month}-${day}T${raw.slice(0, 5)}`;
      }
      startLabel.textContent = "Starts";
    }
  };
  const publish = document.querySelector('button[name="action"][value="publish"]');
  const applyPublish = () => {
    if (!publish) return;
    publish.textContent = voteToggle.checked ? "Open vote" : "Post now";
  };
  voteToggle.addEventListener("change", () => {
    applyStart();
    applyPublish();
  });
  applyStart();
  applyPublish();
}

document.querySelectorAll("[data-q-editor]").forEach((editor) => {
  const cards = [...editor.querySelectorAll("[data-q-card]")];
  const addQuestion = editor.querySelector("[data-add-question]");
  const sync = () => {
    if (addQuestion) addQuestion.hidden = cards.every((card) => !card.hidden);
    for (const card of cards) {
      const choices = [...card.querySelectorAll("[data-q-choices] input")];
      const addChoice = card.querySelector("[data-add-choice]");
      if (addChoice) addChoice.hidden = choices.every((input) => !input.hidden);
    }
  };
  addQuestion?.addEventListener("click", () => {
    const next = cards.find((card) => card.hidden);
    if (!next) return;
    next.hidden = false;
    [...next.querySelectorAll("[data-q-choices] input")].forEach((input, index) => {
      input.hidden = index >= 2;
    });
    next.querySelector("input")?.focus();
    sync();
  });
  for (const card of cards) {
    card.querySelector("[data-add-choice]")?.addEventListener("click", () => {
      const next = [...card.querySelectorAll("[data-q-choices] input")].find((input) => input.hidden);
      if (!next) return;
      next.hidden = false;
      next.focus();
      sync();
    });
  }
  sync();
});

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
