import type { Language } from '../api';

const LANGUAGES: { value: Language; label: string }[] = [
  { value: 'bn', label: 'Bengali' },
  { value: 'en', label: 'English' },
];

function isLanguage(value: string): value is Language {
  return LANGUAGES.some((option) => option.value === value);
}

export function LanguageSelect(props: {
  value: Language;
  disabled: boolean;
  onChange: (language: Language) => void;
  className?: string;
}) {
  return (
    <label className={props.className}>
      Language
      <select
        value={props.value}
        disabled={props.disabled}
        onChange={(event) => isLanguage(event.target.value) && props.onChange(event.target.value)}
      >
        {LANGUAGES.map(({ value, label }) => (
          <option key={value} value={value}>
            {label}
          </option>
        ))}
      </select>
    </label>
  );
}
