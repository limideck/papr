import { useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { useAuth } from "../auth";
import { errorText } from "../lib/errors";
import { NO_AUTOCORRECT } from "../lib/inputProps";
import Icon from "./Icon";

export default function LoginPage() {
  const { t } = useTranslation();
  const { login } = useAuth();
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

  const onSubmit = async (e: FormEvent) => {
    e.preventDefault();
    if (!username.trim() || !password) return;
    setLoading(true);
    setError("");
    try {
      await login(username.trim(), password);
    } catch (err) {
      setError(errorText(err) || t("login.failed"));
      setLoading(false);
    }
  };

  return (
    <div className="login-page">
      <div className="login-bg" aria-hidden="true" />
      <form className="login-card" onSubmit={onSubmit}>
        <div className="login-brand">
          <div className="login-mark">
            <Icon name="papr" size={22} color="#fff" />
          </div> 
          <p>{t("login.subtitle")}</p>
        </div>

        <label className="login-field">
          <span>{t("login.username")}</span>
          <input
            type="text"
            value={username}
            onChange={(e) => setUsername(e.target.value)}
            autoFocus
            {...NO_AUTOCORRECT}
            autoComplete="username"
            placeholder={t("login.usernamePlaceholder")}
          />
        </label>

        <label className="login-field">
          <span>{t("login.password")}</span>
          <input
            type="password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            autoComplete="current-password"
            placeholder={t("login.passwordPlaceholder")}
          />
        </label>

        {error && (
          <div className="login-error" role="alert">
            {error}
          </div>
        )}

        <button
          type="submit"
          className="login-submit"
          disabled={loading || !username.trim() || !password}
        >
          {loading ? t("login.signingIn") : t("login.signIn")}
        </button>
      </form>
    </div>
  );
}
