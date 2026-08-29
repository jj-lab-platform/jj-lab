import { mount } from 'svelte'
import App from './App.svelte'
import './app.css'
import { initTheme } from '$lib/stores/theme.svelte'

initTheme()

const target = document.getElementById('app')
if (!target) throw new Error('#app element not found')
mount(App, { target })
