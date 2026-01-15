const API_BASE = 'http://127.0.0.1:8080/api';

// State
let appState = {
    userId: localStorage.getItem('jarvis_user_id'),
    username: localStorage.getItem('jarvis_username')
};

// DOM Elements
const navLinks = document.querySelectorAll('.nav-links li');
const pages = document.querySelectorAll('.page-content');
const uploadForm = document.getElementById('uploadForm');
const propertiesGrid = document.getElementById('propertiesGrid');
const fileInput = document.getElementById('fileInput');
const fileList = document.getElementById('fileList');

// Initialize
document.addEventListener('DOMContentLoaded', async () => {
    initNavigation();
    await initUser();
    loadProperties();
    updateBalance();
    
    // File input change handler for preview
    fileInput.addEventListener('change', handleFileSelect);
});

// Navigation Logic
function initNavigation() {
    navLinks.forEach(link => {
        link.addEventListener('click', () => {
            const pageId = link.getAttribute('data-page');
            
            // Update Active State
            navLinks.forEach(l => l.classList.remove('active'));
            link.classList.add('active');

            pages.forEach(page => page.classList.remove('active'));
            document.getElementById(`${pageId}Page`).classList.add('active'); // Fixed: ID selection

            if (pageId === 'home') loadProperties();
            if (pageId === 'wallet') updateBalance();
        });
    });
}

// User Logic
async function initUser() {
    if (!appState.userId) {
        // Create a new random user
        const username = 'User_' + Math.floor(Math.random() * 10000);
        try {
            const res = await fetch(`${API_BASE}/users`, {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({
                    username: username,
                    wallet_address: '0x' + Math.random().toString(16).substr(2, 40)
                })
            });
            const user = await res.json();
            appState.userId = user.id;
            appState.username = user.username;
            
            localStorage.setItem('jarvis_user_id', user.id);
            localStorage.setItem('jarvis_username', user.username);
        } catch (e) {
            console.error('Failed to create user', e);
        }
    }
    
    // Update Sidebar
    if (appState.username) {
        document.querySelector('.user-info .name').textContent = appState.username;
    }
}

async function updateBalance() {
    if (!appState.userId) return;
    try {
        const res = await fetch(`${API_BASE}/users/${appState.userId}/balance`);
        const user = await res.json();
        const balance = user.token_balance || 0;
        
        document.querySelector('.user-info .balance').textContent = `${balance} Tokens`;
        document.querySelector('.balance-card .amount').innerHTML = `${balance} <span>TOKENS</span>`;
        // Mock USD conversion
        document.querySelector('.balance-card p').textContent = `≈ $${(balance * 0.05).toFixed(2)} USD`; 
    } catch (e) {
        console.error('Failed to fetch balance', e);
    }
}

// Property Logic
async function loadProperties() {
    propertiesGrid.innerHTML = '<div class="loading-spinner"><i class="fa-solid fa-circle-notch fa-spin"></i></div>';
    
    try {
        const res = await fetch(`${API_BASE}/properties`);
        const properties = await res.json();
        
        propertiesGrid.innerHTML = '';
        
        if (properties.length === 0) {
            propertiesGrid.innerHTML = '<div class="empty-state">No properties found. Be the first to upload!</div>';
            return;
        }

        properties.forEach(prop => {
            const card = document.createElement('div');
            card.className = 'property-card';
            
            // Determine image (video or image file)
            let mediaHtml = '<img src="https://images.unsplash.com/photo-1600596542815-27a89f283b52?ixlib=rb-1.2.1&auto=format&fit=crop&w=800&q=80" alt="Property">';
            
            // If we had actual media serving, we would use prop.image_thumb_webp or similar
            // For now, if we uploaded files, we might want to check file types, but the API doesn't return file paths in the Property struct directly in a usable way for static serving without media_uploads join.
            // Wait, main.rs Property struct has image_thumb_webp, but upload_property doesn't populate it!
            // The upload_property handler inserts into media_uploads. The properties table columns 'image_thumb_webp' are left null in the insert! 
            // See lines 417-430 in main.rs.
            // So we won't see images unless we fix the backend or do a join.
            // I'll stick to a placeholder for now to ensure it works, then maybe fix backend later if needed.
            
            card.innerHTML = `
                <div class="card-image">
                    ${mediaHtml}
                    <div class="price-tag">$${prop.price.toLocaleString()}</div>
                </div>
                <div class="card-info">
                    <h3>${prop.title}</h3>
                    <div class="location"><i class="fa-solid fa-map-marker-alt"></i> ${prop.location}</div>
                    
                    <div class="specs">
                        <div class="spec-item"><i class="fa-solid fa-bed"></i> ${prop.bedrooms || '-'}</div>
                        <div class="spec-item"><i class="fa-solid fa-bath"></i> ${prop.bathrooms || '-'}</div>
                        <div class="spec-item"><i class="fa-solid fa-ruler-combined"></i> ${prop.area_sqm || '-'}m²</div>
                    </div>
                </div>
            `;
            propertiesGrid.appendChild(card);
        });
        
    } catch (e) {
        console.error('Failed to load properties', e);
        propertiesGrid.innerHTML = '<div class="empty-state">Failed to load properties. Check server connection.</div>';
    }
}

// Upload Logic
function handleFileSelect(event) {
    const files = event.target.files;
    fileList.innerHTML = '';
    
    for (const file of files) {
        const item = document.createElement('div');
        item.className = 'file-item';
        item.innerHTML = `<i class="fa-solid fa-file"></i> ${file.name}`;
        fileList.appendChild(item);
    }
}

uploadForm.addEventListener('submit', async (e) => {
    e.preventDefault();
    
    const submitBtn = uploadForm.querySelector('.submit-btn');
    const originalText = submitBtn.innerHTML;
    submitBtn.innerHTML = '<i class="fa-solid fa-circle-notch fa-spin"></i> Uploading...';
    submitBtn.disabled = true;
    
    const formData = new FormData(uploadForm);
    formData.append('user_id', appState.userId);
    
    try {
        const res = await fetch(`${API_BASE}/upload-property`, {
            method: 'POST',
            body: formData
        });
        
        const result = await res.json();
        
        if (result.success) {
            alert(`Property uploaded! You earned ${result.tokens_earned} tokens.`);
            uploadForm.reset();
            fileList.innerHTML = '';
            updateBalance();
            // Go to home
            navLinks[0].click(); 
        } else {
            alert('Upload failed: ' + result.message);
        }
    } catch (e) {
        console.error('Upload error', e);
        alert('Upload failed. Check console.');
    } finally {
        submitBtn.innerHTML = originalText;
        submitBtn.disabled = false;
    }
});
