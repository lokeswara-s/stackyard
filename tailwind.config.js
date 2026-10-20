/** Design tokens extracted from the Stackyard UI mocks (app-mock-html). */
/** @type {import('tailwindcss').Config} */
export default {
  darkMode: "class",
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
          "colors": {
                "surface-container-lowest": "#0a0e14",
                "tertiary-fixed-dim": "#ffb95f",
                "surface-tint": "#adc6ff",
                "surface-container-high": "#262a31",
                "on-secondary-container": "#00311f",
                "outline-variant": "#424754",
                "outline": "#8c909f",
                "surface-container": "#1c2026",
                "on-surface-variant": "#c2c6d6",
                "inverse-primary": "#005ac2",
                "on-secondary-fixed": "#002113",
                "surface-container-highest": "#31353c",
                "error-container": "#93000a",
                "on-primary": "#002e6a",
                "on-primary-fixed-variant": "#004395",
                "on-error": "#690005",
                "on-secondary": "#003824",
                "secondary-fixed-dim": "#4edea3",
                "on-tertiary-fixed-variant": "#653e00",
                "on-error-container": "#ffdad6",
                "primary-fixed-dim": "#adc6ff",
                "error": "#ffb4ab",
                "on-secondary-fixed-variant": "#005236",
                "surface-variant": "#31353c",
                "inverse-on-surface": "#2d3137",
                "on-tertiary-container": "#3e2400",
                "on-tertiary": "#472a00",
                "inverse-surface": "#dfe2eb",
                "surface-bright": "#353940",
                "primary-container": "#4d8eff",
                "tertiary": "#ffb95f",
                "on-surface": "#dfe2eb",
                "surface-dim": "#10141a",
                "on-background": "#dfe2eb",
                "on-primary-fixed": "#001a42",
                "secondary-fixed": "#6ffbbe",
                "surface": "#10141a",
                "tertiary-fixed": "#ffddb8",
                "on-primary-container": "#00285d",
                "secondary": "#4edea3",
                "on-tertiary-fixed": "#2a1700",
                "surface-container-low": "#181c22",
                "primary-fixed": "#d8e2ff",
                "background": "#10141a",
                "tertiary-container": "#ca8100",
                "secondary-container": "#00a572",
                "primary": "#adc6ff"
          },
          "borderRadius": {
                "DEFAULT": "0.25rem",
                "lg": "0.5rem",
                "xl": "0.75rem",
                "full": "9999px"
          },
          "spacing": {
                "space-lg": "1rem",
                "space-sm": "0.5rem",
                "space-md": "0.75rem",
                "space-xs": "0.25rem",
                "space-xl": "1.5rem",
                "gutter": "0.75rem",
                "margin": "1rem"
          },
          "fontFamily": {
                "body-md": [
                      "Inter",
                      "ui-sans-serif",
                      "system-ui",
                      "Segoe UI",
                      "sans-serif"
                ],
                "code-dense": [
                      "JetBrains Mono",
                      "ui-monospace",
                      "SFMono-Regular",
                      "Menlo",
                      "Consolas",
                      "monospace"
                ],
                "label-md": [
                      "JetBrains Mono",
                      "ui-monospace",
                      "SFMono-Regular",
                      "Menlo",
                      "Consolas",
                      "monospace"
                ],
                "body-sm": [
                      "Inter",
                      "ui-sans-serif",
                      "system-ui",
                      "Segoe UI",
                      "sans-serif"
                ],
                "headline-sm": [
                      "Inter",
                      "ui-sans-serif",
                      "system-ui",
                      "Segoe UI",
                      "sans-serif"
                ],
                "headline-md": [
                      "Inter",
                      "ui-sans-serif",
                      "system-ui",
                      "Segoe UI",
                      "sans-serif"
                ],
                "label-sm": [
                      "JetBrains Mono",
                      "ui-monospace",
                      "SFMono-Regular",
                      "Menlo",
                      "Consolas",
                      "monospace"
                ],
                "headline-lg": [
                      "Inter",
                      "ui-sans-serif",
                      "system-ui",
                      "Segoe UI",
                      "sans-serif"
                ]
          },
          "fontSize": {
                "body-md": [
                      "13px",
                      {
                            "lineHeight": "18px",
                            "letterSpacing": "0em",
                            "fontWeight": "400"
                      }
                ],
                "code-dense": [
                      "10px",
                      {
                            "lineHeight": "14px",
                            "letterSpacing": "0em",
                            "fontWeight": "400"
                      }
                ],
                "label-md": [
                      "12px",
                      {
                            "lineHeight": "16px",
                            "letterSpacing": "-0.02em",
                            "fontWeight": "500"
                      }
                ],
                "body-sm": [
                      "12px",
                      {
                            "lineHeight": "16px",
                            "letterSpacing": "0em",
                            "fontWeight": "400"
                      }
                ],
                "headline-sm": [
                      "14px",
                      {
                            "lineHeight": "20px",
                            "letterSpacing": "-0.006em",
                            "fontWeight": "600"
                      }
                ],
                "headline-md": [
                      "16px",
                      {
                            "lineHeight": "24px",
                            "letterSpacing": "-0.011em",
                            "fontWeight": "600"
                      }
                ],
                "label-sm": [
                      "11px",
                      {
                            "lineHeight": "14px",
                            "letterSpacing": "-0.01em",
                            "fontWeight": "500"
                      }
                ],
                "headline-lg": [
                      "20px",
                      {
                            "lineHeight": "28px",
                            "letterSpacing": "-0.015em",
                            "fontWeight": "600"
                      }
                ]
          }
    },
  },
  plugins: [],
};
