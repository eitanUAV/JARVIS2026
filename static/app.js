// JARVIS2026 front-end.
// Talks to the API on its own origin, so the same bundle works on localhost
// and on a deployed host without a rebuild.

const API_BASE = (window.JARVIS_API_BASE || '') + '/api';

// Inline so a listing with no photo renders offline and without sending every
// visitor's IP to a third-party image host.
const PLACEHOLDER_IMAGE =
    'data:image/svg+xml;charset=UTF-8,' +
    encodeURIComponent(
        '<svg xmlns="http://www.w3.org/2000/svg" width="800" height="600" viewBox="0 0 800 600">' +
        '<rect width="800" height="600" fill="#20212b"/>' +
        '<path d="M300 380h200v-40l-55-70-50 60-30-35-65 85z" fill="#2d303e"/>' +
        '<circle cx="345" cy="255" r="26" fill="#2d303e"/>' +
        '<text x="400" y="450" text-anchor="middle" font-family="Outfit,sans-serif" ' +
        'font-size="26" fill="#5b5d70">No photo provided</text></svg>'
    );

const appState = {
    userId: localStorage.getItem('jarvis_user_id'),
    username: localStorage.getItem('jarvis_username'),
    apiKey: localStorage.getItem('jarvis_api_key') || ''
};

// DOM
const navLinks = document.querySelectorAll('.nav-links li');
const pages = document.querySelectorAll('.page-content');
const uploadForm = document.getElementById('uploadForm');
const propertiesGrid = document.getElementById('propertiesGrid');
const fileInput = document.getElementById('fileInput');
const fileList = document.getElementById('fileList');
const searchInput = document.getElementById('searchInput');
const settingsBtn = document.getElementById('settingsBtn');

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Property text comes from whoever uploaded it. Interpolating it straight
/// into innerHTML lets a title like `<img onerror=...>` run script for every
/// visitor, so all untrusted values go through here.
function escapeHtml(value) {
    return String(value ?? '').replace(/[&<>"']/g, (ch) => ({
        '&': '&amp;',
        '<': '&lt;',
        '>': '&gt;',
        '"': '&quot;',
        "'": '&#39;'
    })[ch]);
}

function formatPrice(value) {
    const num = Number(value);
    if (!Number.isFinite(num)) return '—';
    return '$' + num.toLocaleString();
}

function authHeaders(extra = {}) {
    return appState.apiKey ? { ...extra, 'X-API-Key': appState.apiKey } : { ...extra };
}

function toast(message, kind = 'info') {
    let host = document.getElementById('toastHost');
    if (!host) {
        host = document.createElement('div');
        host.id = 'toastHost';
        host.className = 'toast-host';
        document.body.appendChild(host);
    }
    const el = document.createElement('div');
    el.className = `toast toast-${kind}`;
    el.textContent = message;
    host.appendChild(el);
    setTimeout(() => el.remove(), 5000);
}

/// Surfaces the server's own error text instead of a generic failure message.
async function readError(response) {
    try {
        const body = await response.json();
        return body.error || body.message || `Request failed (${response.status})`;
    } catch {
        return `Request failed (${response.status})`;
    }
}

// ---------------------------------------------------------------------------
// Navigation
// ---------------------------------------------------------------------------

function initNavigation() {
    navLinks.forEach((link) => {
        link.addEventListener('click', () => {
            const pageId = link.getAttribute('data-page');

            navLinks.forEach((l) => l.classList.remove('active'));
            link.classList.add('active');

            pages.forEach((page) => page.classList.remove('active'));
            document.getElementById(`${pageId}Page`).classList.add('active');

            if (pageId === 'home') loadProperties();
            if (pageId === 'wallet') {
                updateBalance();
                loadTransactions();
            }
        });
    });
}

// ---------------------------------------------------------------------------
// User
// ---------------------------------------------------------------------------

async function initUser() {
    if (!appState.userId) {
        const username = 'User_' + Math.floor(Math.random() * 100000);
        try {
            const res = await fetch(`${API_BASE}/users`, {
                method: 'POST',
                headers: authHeaders({ 'Content-Type': 'application/json' }),
                body: JSON.stringify({
                    username,
                    wallet_address: '0x' + Math.random().toString(16).slice(2, 42)
                })
            });

            if (!res.ok) {
                toast(await readError(res), 'error');
                return;
            }

            const user = await res.json();
            appState.userId = user.id;
            appState.username = user.username;
            localStorage.setItem('jarvis_user_id', user.id);
            localStorage.setItem('jarvis_username', user.username);
        } catch (e) {
            console.error('Failed to create user', e);
            toast('Could not reach the server.', 'error');
        }
    }

    if (appState.username) {
        document.querySelector('.user-info .name').textContent = appState.username;
    }
}

async function updateBalance() {
    if (!appState.userId) return;
    try {
        const res = await fetch(`${API_BASE}/users/${appState.userId}/balance`);
        if (!res.ok) return;
        const user = await res.json();
        const balance = user.token_balance || 0;

        document.querySelector('.user-info .balance').textContent = `${balance} Tokens`;
        const amount = document.querySelector('.balance-card .amount');
        amount.textContent = '';
        amount.append(document.createTextNode(`${balance} `));
        const unit = document.createElement('span');
        unit.textContent = 'TOKENS';
        amount.appendChild(unit);

        document.querySelector('.balance-card p').textContent =
            `≈ $${(balance * 0.05).toFixed(2)} USD`;
    } catch (e) {
        console.error('Failed to fetch balance', e);
    }
}

async function loadTransactions() {
    const container = document.querySelector('.transactions-list');
    if (!container || !appState.userId) return;

    const heading = container.querySelector('h3');
    container.textContent = '';
    if (heading) container.appendChild(heading);

    try {
        const res = await fetch(`${API_BASE}/users/${appState.userId}/transactions`);
        if (!res.ok) throw new Error(await readError(res));
        const rows = await res.json();

        if (!rows.length) {
            const empty = document.createElement('div');
            empty.className = 'empty-state';
            empty.textContent = 'No transactions yet';
            container.appendChild(empty);
            return;
        }

        rows.forEach((tx) => {
            const row = document.createElement('div');
            row.className = 'transaction-row';
            const when = tx.created_at ? new Date(tx.created_at).toLocaleString() : '';
            row.innerHTML = `
                <div class="tx-type">${escapeHtml(tx.transaction_type)}</div>
                <div class="tx-date">${escapeHtml(when)}</div>
                <div class="tx-amount ${tx.amount >= 0 ? 'positive' : 'negative'}">
                    ${tx.amount >= 0 ? '+' : ''}${escapeHtml(tx.amount)}
                </div>`;
            container.appendChild(row);
        });
    } catch (e) {
        console.error('Failed to load transactions', e);
        const err = document.createElement('div');
        err.className = 'empty-state';
        err.textContent = 'Could not load transactions.';
        container.appendChild(err);
    }
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

function renderProperties(properties) {
    propertiesGrid.textContent = '';

    if (!properties.length) {
        const empty = document.createElement('div');
        empty.className = 'empty-state';
        empty.textContent = 'No properties found.';
        propertiesGrid.appendChild(empty);
        return;
    }

    properties.forEach((prop) => {
        const card = document.createElement('div');
        card.className = 'property-card';

        // Show what the uploader actually submitted. The API returns the
        // attached media plus the cover image columns; the stock photo is only
        // a last resort for listings with no media at all.
        const media = Array.isArray(prop.media) ? prop.media : [];
        const firstImage = media.find((m) => m.file_type === 'image');
        const firstVideo = media.find((m) => m.file_type === 'video');

        let mediaHtml;
        if (firstImage) {
            mediaHtml = `<img src="${encodeURI(firstImage.url)}" alt="${escapeHtml(prop.title)}" loading="lazy">`;
        } else if (firstVideo) {
            mediaHtml = `<video src="${encodeURI(firstVideo.url)}" muted loop playsinline preload="metadata"></video>`;
        } else if (prop.image_thumb_webp) {
            mediaHtml = `<img src="${encodeURI(prop.image_thumb_webp)}" alt="${escapeHtml(prop.title)}" loading="lazy">`;
        } else {
            mediaHtml = `<img src="${PLACEHOLDER_IMAGE}" alt="No photo provided" loading="lazy">`;
        }

        const extra = media.length > 1
            ? `<div class="media-count"><i class="fa-solid fa-images"></i> ${media.length}</div>`
            : '';

        card.innerHTML = `
            <div class="card-image">
                ${mediaHtml}
                <div class="price-tag">${escapeHtml(formatPrice(prop.price))}</div>
                ${extra}
            </div>
            <div class="card-info">
                <h3>${escapeHtml(prop.title)}</h3>
                <div class="location">
                    <i class="fa-solid fa-map-marker-alt"></i> ${escapeHtml(prop.location)}
                </div>
                <div class="specs">
                    <div class="spec-item"><i class="fa-solid fa-bed"></i> ${escapeHtml(prop.bedrooms ?? '-')}</div>
                    <div class="spec-item"><i class="fa-solid fa-bath"></i> ${escapeHtml(prop.bathrooms ?? '-')}</div>
                    <div class="spec-item"><i class="fa-solid fa-ruler-combined"></i> ${escapeHtml(prop.area_sqm ?? '-')}m²</div>
                </div>
            </div>`;
        propertiesGrid.appendChild(card);
    });
}

function showSpinner() {
    propertiesGrid.innerHTML =
        '<div class="loading-spinner"><i class="fa-solid fa-circle-notch fa-spin"></i></div>';
}

async function loadProperties() {
    showSpinner();
    try {
        const res = await fetch(`${API_BASE}/properties`);
        if (!res.ok) throw new Error(await readError(res));
        renderProperties(await res.json());
    } catch (e) {
        console.error('Failed to load properties', e);
        propertiesGrid.innerHTML =
            '<div class="empty-state">Failed to load properties. Check server connection.</div>';
    }
}

/// The search box was previously wired to nothing at all — the /api/search
/// endpoint existed but no client ever called it.
async function runSearch(term) {
    const query = term.trim();
    if (!query) {
        loadProperties();
        return;
    }

    showSpinner();
    try {
        const res = await fetch(`${API_BASE}/search`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ query })
        });
        if (!res.ok) throw new Error(await readError(res));
        renderProperties(await res.json());
    } catch (e) {
        console.error('Search failed', e);
        propertiesGrid.innerHTML = '<div class="empty-state">Search failed.</div>';
    }
}

function debounce(fn, ms) {
    let handle;
    return (...args) => {
        clearTimeout(handle);
        handle = setTimeout(() => fn(...args), ms);
    };
}

// ---------------------------------------------------------------------------
// Upload
// ---------------------------------------------------------------------------

function handleFileSelect(event) {
    fileList.textContent = '';
    for (const file of event.target.files) {
        const item = document.createElement('div');
        item.className = 'file-item';
        const icon = document.createElement('i');
        icon.className = 'fa-solid fa-file';
        item.appendChild(icon);
        // textContent, not innerHTML: the filename is attacker-controlled.
        item.appendChild(document.createTextNode(' ' + file.name));
        fileList.appendChild(item);
    }
}

async function submitUpload(event) {
    event.preventDefault();

    if (!appState.userId) {
        toast('No user session yet. Reload the page.', 'error');
        return;
    }

    const submitBtn = uploadForm.querySelector('.submit-btn');
    const originalText = submitBtn.innerHTML;
    submitBtn.innerHTML = '<i class="fa-solid fa-circle-notch fa-spin"></i> Uploading...';
    submitBtn.disabled = true;

    const formData = new FormData(uploadForm);
    formData.append('user_id', appState.userId);

    try {
        const res = await fetch(`${API_BASE}/upload-property`, {
            method: 'POST',
            headers: authHeaders(),
            body: formData
        });

        if (!res.ok) {
            toast(await readError(res), 'error');
            return;
        }

        const result = await res.json();
        toast(result.message, result.tokens_earned > 0 ? 'success' : 'info');

        // The server reports per-file outcomes; silently dropping them made
        // rejected files look like successful uploads.
        if (result.rejected && result.rejected.length) {
            result.rejected.forEach((r) =>
                toast(`${r.filename}: ${r.reason}`, 'error')
            );
        }

        uploadForm.reset();
        fileList.textContent = '';
        await updateBalance();
        navLinks[0].click();
    } catch (e) {
        console.error('Upload error', e);
        toast('Upload failed. Check the server connection.', 'error');
    } finally {
        submitBtn.innerHTML = originalText;
        submitBtn.disabled = false;
    }
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// When the server runs with API_KEY set, write endpoints need that key.
/// This lets an operator paste it in rather than editing the bundle.
function promptForApiKey() {
    const next = window.prompt(
        'API key for this server (leave blank if the server has no API_KEY set):',
        appState.apiKey
    );
    if (next === null) return;
    appState.apiKey = next.trim();
    if (appState.apiKey) {
        localStorage.setItem('jarvis_api_key', appState.apiKey);
        toast('API key saved for this browser.', 'success');
    } else {
        localStorage.removeItem('jarvis_api_key');
        toast('API key cleared.', 'info');
    }
}

// ---------------------------------------------------------------------------
// Boot
// ---------------------------------------------------------------------------

document.addEventListener('DOMContentLoaded', async () => {
    initNavigation();
    await initUser();
    loadProperties();
    updateBalance();

    fileInput.addEventListener('change', handleFileSelect);
    uploadForm.addEventListener('submit', submitUpload);

    if (searchInput) {
        const debounced = debounce((e) => runSearch(e.target.value), 300);
        searchInput.addEventListener('input', debounced);
        searchInput.addEventListener('keydown', (e) => {
            if (e.key === 'Enter') {
                e.preventDefault();
                runSearch(searchInput.value);
            }
        });
    }

    if (settingsBtn) {
        settingsBtn.addEventListener('click', promptForApiKey);
    }
});
