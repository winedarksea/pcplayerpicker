/** @type {import('tailwindcss').Config} */
module.exports = {
  content: [
    './src/**/*.rs',
    './index.html',
  ],
  darkMode: 'class',
  theme: {
    extend: {
      fontFamily: {
        sans: ['Avenir Next', 'SF Pro Display', 'Segoe UI', 'sans-serif'],
      },
    },
  },
  plugins: [],
}
