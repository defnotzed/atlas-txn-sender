.PHONY: build up down restart logs clean help

.DEFAULT_GOAL := help

# Colors for terminal output
COLOR_RESET=\033[0m
COLOR_CYAN=\033[36m
COLOR_GREEN=\033[32m
COLOR_YELLOW=\033[33m

# Docker compose command
COMPOSE=docker-compose

build: ## Build the Docker image
	@echo "$(COLOR_GREEN)Building Docker image...$(COLOR_RESET)"
	$(COMPOSE) build

up: ## Start the application
	@echo "$(COLOR_GREEN)Starting containers...$(COLOR_RESET)"
	$(COMPOSE) up -d

up-build: ## Build and start the application
	@echo "$(COLOR_GREEN)Building and starting containers...$(COLOR_RESET)"
	$(COMPOSE) up -d --build

down: ## Stop the application
	@echo "$(COLOR_GREEN)Stopping containers...$(COLOR_RESET)"
	$(COMPOSE) down

restart: down up ## Restart the application

logs: ## Show logs from containers
	@echo "$(COLOR_GREEN)Showing logs...$(COLOR_RESET)"
	$(COMPOSE) logs -f

ps: ## List running containers
	@echo "$(COLOR_GREEN)Listing containers...$(COLOR_RESET)"
	$(COMPOSE) ps

clean: down ## Remove containers, volumes, and images
	@echo "$(COLOR_YELLOW)Removing containers, networks, volumes, and images...$(COLOR_RESET)"
	$(COMPOSE) down -v --rmi local

help: ## Display this help message
	@echo "$(COLOR_CYAN)Usage:$(COLOR_RESET)"
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-15s\033[0m %s\n", $$1, $$2}'
