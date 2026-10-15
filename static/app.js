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

const optionList = document.querySelector("#options");
const addOption = document.querySelector("#add-option");
const optionBlank = document.querySelector("#option-blank");
if (optionList && addOption && optionBlank) {
  const countOptions = () => optionList.querySelectorAll("[data-option]").length;
  const nextIndex = () => {
    let max = -1;
    optionList.querySelectorAll("input[name$='_name']").forEach((input) => {
      const match = input.name.match(/^opt(\d+)_name$/);
      if (match) max = Math.max(max, Number(match[1]));
    });
    return max + 1;
  };
  const refreshAdd = () => {
    addOption.hidden = countOptions() >= 8;
  };
  addOption.addEventListener("click", () => {
    if (countOptions() >= 8) return;
    const fragment = optionBlank.content.cloneNode(true);
    const row = fragment.querySelector("[data-option]");
    const n = String(nextIndex());
    row.querySelectorAll("[name]").forEach((el) => {
      el.name = el.name.replace("@@n@@", n);
    });
    optionList.append(fragment);
    row.querySelector("input[name$='_name']")?.focus();
    refreshAdd();
  });
  optionList.addEventListener("click", (event) => {
    const button = event.target.closest(".option-remove");
    if (!button || !optionList.contains(button)) return;
    button.closest("[data-option]")?.remove();
    refreshAdd();
  });
  refreshAdd();
}

const reminderList = document.querySelector("#reminders");
const addReminder = document.querySelector("#add-reminder");
const reminderBlank = document.querySelector("#reminder-blank");
if (reminderList && addReminder && reminderBlank) {
  const countReminders = () => reminderList.querySelectorAll("[data-reminder]").length;
  const nextReminder = () => {
    let max = -1;
    reminderList.querySelectorAll("input[name$='_offset']").forEach((input) => {
      const match = input.name.match(/^rem(\d+)_offset$/);
      if (match) max = Math.max(max, Number(match[1]));
    });
    return max + 1;
  };
  const refreshReminders = () => {
    addReminder.hidden = countReminders() >= 5;
  };
  addReminder.addEventListener("click", () => {
    if (countReminders() >= 5) return;
    const fragment = reminderBlank.content.cloneNode(true);
    const row = fragment.querySelector("[data-reminder]");
    const n = String(nextReminder());
    row.querySelectorAll("[name]").forEach((el) => {
      el.name = el.name.replace("@@n@@", n);
    });
    reminderList.append(fragment);
    row.querySelector("input[name$='_offset']")?.focus();
    refreshReminders();
  });
  reminderList.addEventListener("click", (event) => {
    const button = event.target.closest(".reminder-remove");
    if (!button || !reminderList.contains(button)) return;
    button.closest("[data-reminder]")?.remove();
    refreshReminders();
  });
  refreshReminders();
}
