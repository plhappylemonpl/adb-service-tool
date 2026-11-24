/** @type {import('tailwindcss').Config} */
export default {
  content: ['./src/**/*.{html,js,svelte,ts}'],
  theme: {
    extend: {
      colors: {
        // Motyw Drapco - głęboka czerń + złoto
        ink: {
          900: '#0a0a0a',
          800: '#121212',
          700: '#191919',
          600: '#222222',
          500: '#2d2d2d',
          400: '#3a3a3a'
        },
        gold: {
          DEFAULT: '#c9a249',
          soft: '#d9b466',
          dim: '#8a7030'
        },
        ok: '#7fd08a',
        warn: '#d9b466',
        err: '#e08a8a'
      },
      fontFamily: {
        sans: ['Inter', 'Segoe UI', 'Ubuntu', 'Noto Sans', 'Noto Sans CJK SC', 'Microsoft YaHei UI', 'Yu Gothic UI', 'Malgun Gothic', 'sans-serif'],
        mono: ['JetBrains Mono', 'Consolas', 'monospace']
      },
      keyframes: {
        'fade-in': { from: { opacity: '0', transform: 'translateY(4px)' }, to: { opacity: '1', transform: 'none' } },
        'pulse-gold': {
          '0%,100%': { boxShadow: '0 0 0 0 rgba(201,162,73,0.4)' },
          '50%': { boxShadow: '0 0 0 6px rgba(201,162,73,0)' }
        }
      },
      animation: {
        'fade-in': 'fade-in 0.18s ease-out',
        'pulse-gold': 'pulse-gold 1.8s ease-in-out infinite'
      }
    }
  },
  plugins: []
};
